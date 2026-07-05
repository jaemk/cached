use super::{CacheEvict, Cached, DefaultHashBuilder, LruCache};
use crate::{CachedIter, CachedPeek, CloneCached};
use std::hash::{BuildHasher, Hash};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(feature = "async_core")]
use {super::CachedGetOrSetAsync, std::future::Future};

/// Implemented by values stored in [`ExpiringLruCache`] and [`ExpiringCache`](crate::ExpiringCache)
/// so the value itself decides when it is stale. Expired values are not returned by lookups
/// and are removed on access:
///
/// ```rust
/// use cached::{CachedExt, Expires, ExpiringCache, ExpiringLruCache};
///
/// struct Token {
///     #[allow(dead_code)]
///     value: String,
///     expired: bool,
/// }
/// impl Expires for Token {
///     fn is_expired(&self) -> bool {
///         self.expired
///     }
/// }
///
/// // Unbounded store (default for `#[cached(expires = true)]`)
/// let mut cache: ExpiringCache<u32, Token> = ExpiringCache::new();
/// cache.set(1, Token { value: "live".into(), expired: false });
/// assert!(cache.get(&1).is_some());
/// cache.set(2, Token { value: "stale".into(), expired: true });
/// assert!(cache.get(&2).is_none()); // expired -> not returned
///
/// // LRU-bounded store (`#[cached(expires = true, max_size = N)]`)
/// let mut lru: ExpiringLruCache<u32, Token> = ExpiringLruCache::new(8);
/// lru.set(3, Token { value: "live".into(), expired: false });
/// assert!(lru.get(&3).is_some());
/// ```
pub trait Expires {
    /// `is_expired` returns whether the value has expired.
    ///
    /// This is the authoritative liveness check: callers must use `is_expired` to
    /// decide whether a cached value may be returned, not `expires_at`.
    fn is_expired(&self) -> bool;

    /// Returns the [`crate::time::Instant`] at which this value expires, or `None` if the
    /// expiry instant is unknown or not tracked by this type.
    ///
    /// The default implementation returns `None`. Override this in types that record a
    /// concrete deadline to enable observability (logging, metrics) and to allow callers
    /// to extend or compare deadlines without re-computing them.
    ///
    /// `is_expired()` remains the authoritative liveness check; `expires_at` is advisory
    /// and must not be used as a substitute for `is_expired`.
    fn expires_at(&self) -> Option<crate::time::Instant> {
        None
    }
}

/// LRU-bounded cache with per-value expiry.
///
/// Stores values that implement the [`Expires`] trait so that expiration
/// is determined by the values themselves. This is useful for caching
/// values which themselves contain an expiry timestamp.
///
/// For an unbounded variant (no size cap), see [`ExpiringCache`](crate::ExpiringCache).
/// When using the `#[cached]` proc macro, `expires = true` selects this store when `max_size`
/// is also specified; without `max_size`, it selects the unbounded `ExpiringCache`.
///
/// Note: This cache is in-memory only.
///
/// **`cache_size` / `iter` / `evict` contract**: `cache_size()` returns the raw stored entry count
/// and may include expired-but-not-yet-swept entries. `iter()` omits expired entries
/// from the view but does not remove them. Call `evict()` (via [`CacheEvict`](crate::CacheEvict))
/// to physically remove expired entries and obtain an accurate live count.
///
/// Note: once specialization is stable (`#[feature(specialization)]`), the expiry-checking
/// behavior here could be folded into [`LruCache`] via a specialized `Cached<K, V>` impl
/// for `V: Expires`, eliminating this separate type. Until then, the two must remain
/// distinct because overlapping blanket impls are not allowed on stable Rust.
pub struct ExpiringLruCache<K: Hash + Eq, V: Expires, S = DefaultHashBuilder> {
    pub(super) store: LruCache<K, V, S>,
    pub(super) hits: AtomicU64,
    pub(super) misses: AtomicU64,
    pub(super) evictions: AtomicU64,
    pub(super) on_evict: Option<super::OnEvict<K, V>>,
}

impl<K: Hash + Eq, V: Expires, S> std::fmt::Debug for ExpiringLruCache<K, V, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExpiringLruCache")
            .field("hits", &self.hits.load(Ordering::Relaxed))
            .field("misses", &self.misses.load(Ordering::Relaxed))
            .field("evictions", &self.evictions.load(Ordering::Relaxed))
            .field("on_evict", &self.on_evict.as_ref().map(|_| "on_evict"))
            .finish()
    }
}

/// Two `ExpiringLruCache` values are equal when their stored entries are equal
/// (same keys, same values). Equality is membership-based: LRU recency order is
/// not compared. Metrics (hits, misses, evictions) and the `on_evict` callback
/// are not part of the comparison.
impl<K, V, S> PartialEq for ExpiringLruCache<K, V, S>
where
    K: Clone + Hash + Eq,
    V: Expires + PartialEq,
    S: BuildHasher,
{
    fn eq(&self, other: &Self) -> bool {
        self.store == other.store
    }
}

impl<K, V, S> Eq for ExpiringLruCache<K, V, S>
where
    K: Clone + Hash + Eq,
    V: Expires + Eq,
    S: BuildHasher,
{
}

impl<K, V, S> Clone for ExpiringLruCache<K, V, S>
where
    K: Clone + Hash + Eq,
    V: Expires + Clone,
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

/// Builder for [`ExpiringLruCache`].
///
/// Note: there is intentionally **no `.ttl()` setter**. An `ExpiringLruCache` has no global
/// expiry duration -- each value decides when it is expired via the [`Expires`] trait, while
/// `max_size` bounds the entry count via LRU. For a single global TTL applied to every entry,
/// use [`LruTtlCache`](crate::stores::LruTtlCache) instead.
#[doc(alias = "ttl")]
pub struct ExpiringLruCacheBuilder<K, V: Expires, S = DefaultHashBuilder> {
    size: Option<usize>,
    on_evict: Option<super::OnEvict<K, V>>,
    hasher: S,
}

impl<K, V: Expires> Default for ExpiringLruCacheBuilder<K, V, DefaultHashBuilder> {
    fn default() -> Self {
        Self {
            size: None,
            on_evict: None,
            hasher: super::new_default_hash_builder(),
        }
    }
}

impl<K, V: Expires, S> ExpiringLruCacheBuilder<K, V, S> {
    /// Set the maximum number of entries.
    #[doc(alias = "size")]
    #[doc(alias = "capacity")]
    #[must_use]
    pub fn max_size(mut self, max_size: usize) -> Self {
        self.size = Some(max_size);
        self
    }

    /// Set a callback to be invoked when an entry is evicted. The callback fires for:
    /// - LRU capacity eviction: inserting past `max_size` evicts the least-recently-used entry.
    /// - Capacity shrink via [`set_max_size`](ExpiringLruCache::set_max_size) /
    ///   [`try_set_max_size`](ExpiringLruCache::try_set_max_size).
    /// - An expired value encountered during `cache_get`, `cache_get_mut`,
    ///   `cache_get_or_set_with_mut`, `cache_try_get_or_set_with_mut` (the primary
    ///   implementations), `cache_get_or_set_with`, `cache_try_get_or_set_with` (default-impl
    ///   wrappers that delegate to the `_mut` variants), and their async equivalents.
    /// - Overwriting an already-expired entry via [`cache_set`](crate::Cached::cache_set):
    ///   the displaced value is filtered from the return (`None`), so it fires the callback
    ///   and counts an eviction.
    /// - An explicit [`evict`](ExpiringLruCache::evict) sweep.
    /// - Explicit [`cache_remove`](crate::Cached::cache_remove) /
    ///   [`cache_remove_entry`](crate::Cached::cache_remove_entry), including when the removed
    ///   entry was already expired.
    ///
    /// It does **not** fire on [`cache_clear`](crate::Cached::cache_clear) or `cache_reset`.
    /// Use [`cache_clear_with_on_evict`](ExpiringLruCache::cache_clear_with_on_evict)
    /// instead to opt into callback firing and eviction counter increments when clearing
    /// all entries.
    #[must_use]
    pub fn on_evict(mut self, on_evict: impl Fn(&K, &V) + Send + Sync + 'static) -> Self {
        self.on_evict = Some(Arc::new(on_evict));
        self
    }

    /// Switch to a custom hash builder `S2`, returning a builder parameterized on `S2`.
    ///
    /// The hasher is used to hash keys in the internal `LruCache`. Calling this method
    /// changes the builder's type parameter so `build()` returns an `ExpiringLruCache<K, V, S2>`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use cached::{Cached, Expires, ExpiringLruCache};
    /// use std::collections::hash_map::RandomState;
    ///
    /// struct Val(bool);
    /// impl Expires for Val { fn is_expired(&self) -> bool { self.0 } }
    ///
    /// let mut cache = ExpiringLruCache::<u32, Val>::builder()
    ///     .max_size(10)
    ///     .hasher(RandomState::new())
    ///     .build()
    ///     .unwrap();
    /// cache.cache_set(1, Val(false));
    /// assert!(cache.cache_get(&1).is_some());
    /// ```
    #[doc(alias = "with_hasher")]
    #[must_use]
    pub fn hasher<S2: BuildHasher>(self, hasher: S2) -> ExpiringLruCacheBuilder<K, V, S2> {
        ExpiringLruCacheBuilder {
            size: self.size,
            on_evict: self.on_evict,
            hasher,
        }
    }

    /// Build the cache.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError::MissingRequired`](super::BuildError) if `max_size` was not set,
    /// or [`BuildError::InvalidValue`](super::BuildError) if `max_size` is `0`.
    pub fn build(self) -> Result<ExpiringLruCache<K, V, S>, super::BuildError>
    where
        K: Hash + Eq + Clone,
        S: BuildHasher,
    {
        let size = self
            .size
            .ok_or(super::BuildError::MissingRequired("max_size"))?;
        let mut store = LruCache::builder()
            .max_size(size)
            .hasher(self.hasher)
            .build()?;
        store.disable_hit_miss_tracking();
        // Two separate callbacks for two separate eviction causes:
        //   cache.on_evict    -- fires when ExpiringLruCache itself removes an expired entry
        //   cache.store.on_evict -- fires when LruCache::check_capacity evicts for capacity
        // Both must be registered independently so neither path is silently skipped.
        let mut cache = ExpiringLruCache {
            store,
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            evictions: AtomicU64::new(0),
            on_evict: self.on_evict.clone(),
        };
        if let Some(on_evict) = self.on_evict {
            cache.store.on_evict = Some(on_evict);
        }
        Ok(cache)
    }
}

impl<K: Clone + Hash + Eq, V: Expires> ExpiringLruCache<K, V> {
    /// Construct a ready-to-use [`ExpiringLruCache`] holding up to `max_size` entries.
    ///
    /// For optional settings (`on_evict`) use [`builder`](Self::builder).
    ///
    /// # Panics
    ///
    /// Panics if `max_size` is `0`, or if pre-allocating the backing store for
    /// `max_size` entries fails (e.g. `usize::MAX`). Use [`builder`](Self::builder)
    /// with [`build`](ExpiringLruCacheBuilder::build) to handle those cases without panicking.
    #[must_use]
    pub fn new(max_size: usize) -> Self {
        Self::builder()
            .max_size(max_size)
            .build()
            .expect("ExpiringLruCache::new requires a non-zero max_size with a valid allocation")
    }

    /// Return a builder for constructing an [`ExpiringLruCache`].
    #[must_use]
    pub fn builder() -> ExpiringLruCacheBuilder<K, V> {
        ExpiringLruCacheBuilder::default()
    }
}

impl<K: Clone + Hash + Eq, V: Expires, S: BuildHasher> ExpiringLruCache<K, V, S> {
    /// Returns the maximum number of entries this cache will hold before evicting.
    ///
    /// This is the bound set via [`ExpiringLruCacheBuilder::max_size`],
    /// not the current number of entries — use [`cache_size`](crate::Cached::cache_size) for that.
    #[doc(alias = "size")]
    #[doc(alias = "max_size")]
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.store.capacity()
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
    /// Panics if `max_size` is 0. Use [`try_set_max_size`](ExpiringLruCache::try_set_max_size)
    /// to validate first and avoid the panic.
    pub fn set_max_size(&mut self, max_size: usize) -> Option<usize> {
        self.store.set_max_size(max_size)
    }

    /// Fallible counterpart of [`set_max_size`](ExpiringLruCache::set_max_size): validates
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
        self.store.try_set_max_size(max_size)
    }

    /// Evict expired values from the cache.
    #[must_use]
    pub fn evict(&mut self) -> usize {
        let on_evict = &self.on_evict;
        let evictions = &self.evictions;
        let mut removed = 0;
        self.store.retain_silent(|key, value| {
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
            for (k, v) in &removed {
                on_evict(k, v);
            }
        }
    }
}

// https://docs.rs/cached/latest/cached/trait.Cached.html
impl<K: Hash + Eq + Clone, V: Expires, S: BuildHasher> Cached<K, V> for ExpiringLruCache<K, V, S> {
    type Error = std::convert::Infallible;

    fn cache_get<Q>(&mut self, k: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        let hash = self.store.hash(k);
        if let Some(index) = self.store.get_index(hash, k) {
            let value = &self.store.order.get(index).1;
            if !value.is_expired() {
                self.store.order.move_to_front(index);
                self.hits.fetch_add(1, Ordering::Relaxed);
                Some(&self.store.order.get(index).1)
            } else {
                self.misses.fetch_add(1, Ordering::Relaxed);
                if let Some((key, old)) = self.store.pop_raw(k) {
                    if let Some(on_evict) = &self.on_evict {
                        on_evict(&key, &old);
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

    fn cache_get_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        let hash = self.store.hash(key);
        if let Some(index) = self.store.get_index(hash, key) {
            let value = &self.store.order.get(index).1;
            if !value.is_expired() {
                self.store.order.move_to_front(index);
                self.hits.fetch_add(1, Ordering::Relaxed);
                Some(&mut self.store.order.get_mut(index).1)
            } else {
                self.misses.fetch_add(1, Ordering::Relaxed);
                if let Some((k, old)) = self.store.pop_raw(key) {
                    if let Some(on_evict) = &self.on_evict {
                        on_evict(&k, &old);
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

    fn cache_get_or_set_with_mut<F: FnOnce() -> V>(&mut self, k: K, f: F) -> &mut V {
        let key_for_evict = k.clone();
        // get_or_set_with_if will set the value in the cache if an existing
        // value is not valid, which, in our case, is if the value has expired.
        let (was_present, was_valid, old_val, v) =
            self.store.get_or_set_with_if(k, f, |v| !v.is_expired());
        if was_present && was_valid {
            self.hits.fetch_add(1, Ordering::Relaxed);
        } else {
            if let Some(old) = old_val {
                if let Some(on_evict) = &self.on_evict {
                    on_evict(&key_for_evict, &old);
                }
                self.evictions.fetch_add(1, Ordering::Relaxed);
            }
            self.misses.fetch_add(1, Ordering::Relaxed);
        }
        v
    }
    fn cache_try_get_or_set_with_mut<F: FnOnce() -> Result<V, E>, E>(
        &mut self,
        key: K,
        f: F,
    ) -> Result<&mut V, E> {
        let key_for_evict = key.clone();
        // Count the miss the instant the factory runs. The inner store calls it only on a miss
        // (vacant slot or expired entry), so a hit never counts one; and because the increment
        // lands before `f` returns, an `Err` still records the miss instead of losing it on the
        // `?` early return. This matches `ExpiringCache`'s try-path accounting (EXP-2).
        let counted_f = {
            let misses = &self.misses;
            move || {
                misses.fetch_add(1, Ordering::Relaxed);
                f()
            }
        };
        let (was_present, was_valid, old_val, v) =
            self.store
                .try_get_or_set_with_if(key, counted_f, |v| !v.is_expired())?;
        if was_present && was_valid {
            self.hits.fetch_add(1, Ordering::Relaxed);
        } else if let Some(old) = old_val {
            if let Some(on_evict) = &self.on_evict {
                on_evict(&key_for_evict, &old);
            }
            self.evictions.fetch_add(1, Ordering::Relaxed);
        }
        Ok(v)
    }
    fn cache_set(&mut self, k: K, v: V) -> Option<V> {
        // Clone the key only when a callback might need it. The inner `LruCache::cache_set` does
        // not fire `on_evict` on an overwrite, so an expired displaced value would otherwise be
        // dropped silently; filter it from the return and fire `on_evict` + count once here.
        let key_for_evict = self.on_evict.as_ref().map(|_| k.clone());
        match self.store.cache_set(k, v) {
            Some(old) if old.is_expired() => {
                if let (Some(on_evict), Some(key)) = (&self.on_evict, &key_for_evict) {
                    on_evict(key, &old);
                }
                self.evictions.fetch_add(1, Ordering::Relaxed);
                None
            }
            other => other,
        }
    }
    fn cache_remove<Q>(&mut self, k: &Q) -> Option<V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        self.cache_remove_entry(k)
            .and_then(|(_, v)| if v.is_expired() { None } else { Some(v) })
    }

    fn cache_remove_entry<Q>(&mut self, k: &Q) -> Option<(K, V)>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        if let Some((stored_k, v)) = self.store.pop_raw(k) {
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
        // Entries are dropped in-place; `on_evict` is NOT called for cleared entries.
        // Delegate to the inner LruCache's reset which preserves the hash builder.
        self.store.cache_reset();
        self.cache_reset_metrics();
    }
    fn cache_size(&self) -> usize {
        self.store.cache_size()
    }
    fn cache_capacity(&self) -> Option<usize> {
        // Bounded by the inner `LruCache`; report it like the other bounded
        // stores so `metrics().capacity` is accurate.
        self.store.cache_capacity()
    }
    fn cache_hits(&self) -> Option<u64> {
        Some(self.hits.load(Ordering::Relaxed))
    }
    fn cache_misses(&self) -> Option<u64> {
        Some(self.misses.load(Ordering::Relaxed))
    }
    fn cache_evictions(&self) -> Option<u64> {
        Some(self.evictions.load(Ordering::Relaxed) + self.store.cache_evictions().unwrap_or(0))
    }
    fn cache_reset_metrics(&mut self) {
        self.hits.store(0, Ordering::Relaxed);
        self.misses.store(0, Ordering::Relaxed);
        self.evictions.store(0, Ordering::Relaxed);
        self.store.cache_reset_metrics();
    }
}

impl<K: Hash + Eq + Clone, V: Expires, S: BuildHasher> CachedIter<K, V>
    for ExpiringLruCache<K, V, S>
{
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

impl<K: Hash + Eq + Clone, V: Expires, S: BuildHasher> CachedPeek<K, V>
    for ExpiringLruCache<K, V, S>
{
    fn cache_peek<Q>(&self, key: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        self.store.cache_peek(key).and_then(|value| {
            if value.is_expired() {
                None
            } else {
                Some(value)
            }
        })
    }
}

#[cfg(feature = "async_core")]
impl<K, V, S> CachedGetOrSetAsync<K, V> for ExpiringLruCache<K, V, S>
where
    K: Hash + Eq + Clone + Send,
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
            let key_for_evict = k.clone();
            let (was_present, was_valid, old_val, v) = self
                .store
                .get_or_set_with_if_async(k, f, |v| !v.is_expired())
                .await;
            if was_present && was_valid {
                self.hits.fetch_add(1, Ordering::Relaxed);
            } else {
                if let Some(old) = old_val {
                    if let Some(on_evict) = &self.on_evict {
                        on_evict(&key_for_evict, &old);
                    }
                    self.evictions.fetch_add(1, Ordering::Relaxed);
                }
                self.misses.fetch_add(1, Ordering::Relaxed);
            }
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
            let key_for_evict = k.clone();
            // Count the miss when the factory is invoked (miss path only), so an `Err` from the
            // async factory still records it instead of losing it on the `?` early return.
            // Mirrors the sync try path and `ExpiringCache` (EXP-2).
            let counted_f = {
                let misses = &self.misses;
                move || {
                    misses.fetch_add(1, Ordering::Relaxed);
                    f()
                }
            };
            let (was_present, was_valid, old_val, v) = self
                .store
                .try_get_or_set_with_if_async(k, counted_f, |v| !v.is_expired())
                .await?;
            if was_present && was_valid {
                self.hits.fetch_add(1, Ordering::Relaxed);
            } else if let Some(old) = old_val {
                if let Some(on_evict) = &self.on_evict {
                    on_evict(&key_for_evict, &old);
                }
                self.evictions.fetch_add(1, Ordering::Relaxed);
            }
            Ok(v)
        }
    }
}

impl<K: Hash + Eq + Clone, V: Expires + Clone, S: BuildHasher> CloneCached<K, V>
    for ExpiringLruCache<K, V, S>
{
    fn cache_get_with_expiry_status<Q>(&mut self, k: &Q) -> (Option<V>, bool)
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        let hash = self.store.hash(k);
        if let Some(index) = self.store.get_index(hash, k) {
            let value = &self.store.order.get(index).1;
            let expired = value.is_expired();
            if expired {
                self.misses.fetch_add(1, Ordering::Relaxed);
                // Don't move to front — expired entries must not be promoted.
                // Return the stale value so callers using `result_fallback` can
                // use it during revalidation.
                (Some(self.store.order.get(index).1.clone()), true)
            } else {
                self.store.order.move_to_front(index);
                self.hits.fetch_add(1, Ordering::Relaxed);
                (Some(self.store.order.get(index).1.clone()), false)
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
    /// counters and does not promote in LRU order.
    fn cache_peek_with_expiry_status<Q>(&self, k: &Q) -> (Option<V>, bool)
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
        V: Clone,
    {
        // Use the inner LruCache's `cache_peek` to avoid LRU promotion.
        if let Some(value) = self.store.cache_peek(k) {
            let expired = value.is_expired();
            (Some(value.clone()), expired)
        } else {
            (None, false)
        }
    }
}

impl<K: std::hash::Hash + Eq + Clone, V: Expires, S: BuildHasher> CacheEvict
    for ExpiringLruCache<K, V, S>
{
    fn evict(&mut self) -> usize {
        ExpiringLruCache::evict(self)
    }
}

#[cfg(test)]
/// Expiring Value Cache tests
mod tests {
    use super::*;
    use crate::{Cached, CachedExt};
    use std::sync::atomic::{AtomicU64, Ordering};

    type ExpiredU8 = u8;

    impl Expires for ExpiredU8 {
        fn is_expired(&self) -> bool {
            *self > 10
        }
    }

    #[test]
    fn new_returns_ready_cache_respecting_max_size() {
        let mut c: ExpiringLruCache<u8, ExpiredU8> = ExpiringLruCache::new(2);
        assert_eq!(c.capacity(), 2);
        assert_eq!(c.set(1, 5), None);
        assert_eq!(c.get(&1), Some(&5));
        c.set(2, 6);
        c.set(3, 7); // evicts LRU (1)
        assert_eq!(c.cache_size(), 2);
        assert_eq!(c.get(&1), None);
    }

    #[test]
    #[should_panic(expected = "non-zero max_size")]
    fn new_zero_max_size_panics() {
        let _c: ExpiringLruCache<u8, ExpiredU8> = ExpiringLruCache::new(0);
    }

    #[test]
    fn expiring_value_cache_get_miss() {
        let mut c: ExpiringLruCache<u8, ExpiredU8> =
            ExpiringLruCache::builder().max_size(3).build().unwrap();

        // Getting a non-existent cache key.
        assert!(c.get(&1).is_none());
        assert_eq!(c.cache_hits(), Some(0));
        assert_eq!(c.cache_misses(), Some(1));
    }

    #[test]
    fn cache_set_over_expired_returns_none_fires_on_evict_and_counts() {
        use std::sync::{Arc, Mutex};
        let fired: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(vec![]));
        let fired2 = fired.clone();
        let mut c: ExpiringLruCache<u8, ExpiredU8> = ExpiringLruCache::builder()
            .max_size(4)
            .on_evict(move |k: &u8, _v: &ExpiredU8| fired2.lock().unwrap().push(*k))
            .build()
            .unwrap();
        c.set(1, 15); // expired (>10)
        let before = c.cache_evictions().unwrap();
        // Overwriting an expired value: filtered from the return (None), fires on_evict once,
        // counts one eviction.
        assert_eq!(c.cache_set(1, 3), None);
        assert_eq!(c.cache_evictions(), Some(before + 1));
        assert_eq!(fired.lock().unwrap().clone(), vec![1]);
        // Overwriting a live value returns it, and does not fire on_evict or count.
        assert_eq!(c.cache_set(1, 4), Some(3));
        assert_eq!(c.cache_evictions(), Some(before + 1));
        assert_eq!(fired.lock().unwrap().clone(), vec![1]);
    }

    #[test]
    fn expiring_lru_try_get_or_set_with_err_keeps_expired_and_counts_miss() {
        // EXP-2: on a factory Err over an expired entry, ExpiringLruCache counts a miss the
        // instant the factory runs (matching ExpiringCache) instead of losing it on the `?`
        // early return, and leaves the expired entry in place without firing on_evict.
        let mut c: ExpiringLruCache<u8, ExpiredU8> =
            ExpiringLruCache::builder().max_size(4).build().unwrap();
        c.set(1, 15); // expired (>10)
        let result: Result<&ExpiredU8, &str> = c.cache_try_get_or_set_with(1, || Err("fail"));
        assert!(result.is_err());
        assert_eq!(c.cache_size(), 1, "expired entry must remain after Err");
        assert_eq!(c.cache_evictions(), Some(0));
        assert_eq!(
            c.cache_misses(),
            Some(1),
            "miss must be counted before f() even on Err"
        );
    }

    #[test]
    fn expiring_lru_try_get_or_set_with_ok_evicts_and_counts_miss() {
        let mut c: ExpiringLruCache<u8, ExpiredU8> =
            ExpiringLruCache::builder().max_size(4).build().unwrap();
        c.set(1, 15); // expired
        let result: Result<&ExpiredU8, &str> = c.cache_try_get_or_set_with(1, || Ok(3));
        assert_eq!(*result.unwrap(), 3);
        assert_eq!(c.cache_evictions(), Some(1));
        assert_eq!(c.cache_misses(), Some(1));
    }

    #[test]
    fn expiring_value_cache_reports_capacity() {
        // Regression: `ExpiringLruCache` is size-bounded, so it must report a
        // capacity like the other bounded stores (was falling through to the
        // `Cached` default `None`, making `metrics().capacity` inaccurate).
        let c: ExpiringLruCache<u8, ExpiredU8> =
            ExpiringLruCache::builder().max_size(7).build().unwrap();
        assert_eq!(c.cache_capacity(), Some(7));
        assert_eq!(c.metrics().capacity, Some(7));
    }

    #[test]
    fn capacity_returns_bound_not_live_size() {
        let mut c: ExpiringLruCache<u8, ExpiredU8> =
            ExpiringLruCache::builder().max_size(3).build().unwrap();
        assert_eq!(c.capacity(), 3);
        assert_eq!(c.cache_size(), 0);

        c.cache_set(1, 5);
        c.cache_set(2, 6);
        assert_eq!(c.capacity(), 3);
        assert_eq!(c.cache_size(), 2);

        // Eviction past the bound keeps capacity fixed while live count stays capped.
        c.cache_set(3, 7);
        c.cache_set(4, 8);
        assert_eq!(c.capacity(), 3);
        assert_eq!(c.cache_size(), 3);
    }

    #[test]
    fn builder_rejects_zero_max_size() {
        let result = ExpiringLruCache::<u8, ExpiredU8>::builder()
            .max_size(0)
            .build();
        assert!(result.is_err());
    }

    #[test]
    fn expiring_value_cache_get_hit() {
        let mut c: ExpiringLruCache<u8, ExpiredU8> =
            ExpiringLruCache::builder().max_size(3).build().unwrap();

        // Getting a cached value.
        assert!(c.set(1, 2).is_none());
        assert_eq!(c.get(&1), Some(&2));
        assert_eq!(c.cache_hits(), Some(1));
        assert_eq!(c.cache_misses(), Some(0));
    }

    #[test]
    fn expiring_value_cache_get_expired() {
        let mut c: ExpiringLruCache<u8, ExpiredU8> =
            ExpiringLruCache::builder().max_size(3).build().unwrap();

        assert!(c.set(2, 12).is_none());

        assert!(c.get(&2).is_none());
        assert_eq!(c.cache_hits(), Some(0));
        assert_eq!(c.cache_misses(), Some(1));
    }

    #[test]
    fn expiring_value_cache_get_mut_miss() {
        let mut c: ExpiringLruCache<u8, ExpiredU8> =
            ExpiringLruCache::builder().max_size(3).build().unwrap();

        // Getting a non-existent cache key.
        assert!(c.cache_get_mut(&1).is_none());
        assert_eq!(c.cache_hits(), Some(0));
        assert_eq!(c.cache_misses(), Some(1));
    }

    #[test]
    fn expiring_value_cache_get_mut_hit() {
        let mut c: ExpiringLruCache<u8, ExpiredU8> =
            ExpiringLruCache::builder().max_size(3).build().unwrap();

        // Getting a cached value.
        assert!(c.set(1, 2).is_none());
        assert_eq!(c.cache_get_mut(&1), Some(&mut 2));
        assert_eq!(c.cache_hits(), Some(1));
        assert_eq!(c.cache_misses(), Some(0));
    }

    #[test]
    fn expiring_value_cache_get_mut_expired() {
        let mut c: ExpiringLruCache<u8, ExpiredU8> =
            ExpiringLruCache::builder().max_size(3).build().unwrap();

        assert!(c.set(2, 12).is_none());

        assert!(c.get(&2).is_none());
        assert_eq!(c.cache_hits(), Some(0));
        assert_eq!(c.cache_misses(), Some(1));
    }

    #[test]
    fn expiring_value_cache_get_or_set_with_missing() {
        let mut c: ExpiringLruCache<u8, ExpiredU8> =
            ExpiringLruCache::builder().max_size(3).build().unwrap();

        assert_eq!(c.cache_get_or_set_with(1, || 1), &1);
        assert_eq!(c.cache_hits(), Some(0));
        assert_eq!(c.cache_misses(), Some(1));
    }

    #[test]
    fn expiring_value_cache_get_or_set_with_present() {
        let mut c: ExpiringLruCache<u8, ExpiredU8> =
            ExpiringLruCache::builder().max_size(3).build().unwrap();
        assert!(c.set(1, 5).is_none());

        // Existing value is returned rather than setting new value.
        assert_eq!(c.cache_get_or_set_with(1, || 1), &5);
        assert_eq!(c.cache_hits(), Some(1));
        assert_eq!(c.cache_misses(), Some(0));
    }

    #[test]
    fn expiring_value_cache_get_or_set_with_expired() {
        let mut c: ExpiringLruCache<u8, ExpiredU8> =
            ExpiringLruCache::builder().max_size(3).build().unwrap();
        assert!(c.set(1, 11).is_none());

        // New value is returned as existing had expired.
        assert_eq!(c.cache_get_or_set_with(1, || 1), &1);
        assert_eq!(c.cache_hits(), Some(0));
        assert_eq!(c.cache_misses(), Some(1));
    }

    #[test]
    fn expiring_value_cache_try_get_or_set_with_missing() {
        let mut c: ExpiringLruCache<u8, ExpiredU8> =
            ExpiringLruCache::builder().max_size(3).build().unwrap();

        assert_eq!(c.cache_try_get_or_set_with(1, || Ok::<_, ()>(1)), Ok(&1));
        assert_eq!(c.cache_hits(), Some(0));
        assert_eq!(c.cache_misses(), Some(1));

        assert_eq!(c.cache_try_get_or_set_with(1, || Err(())), Ok(&1));
        assert_eq!(c.cache_hits(), Some(1));
        assert_eq!(c.cache_misses(), Some(1));

        assert_eq!(c.cache_try_get_or_set_with(2, || Ok::<_, ()>(2)), Ok(&2));
        assert_eq!(c.cache_hits(), Some(1));
        assert_eq!(c.cache_misses(), Some(2));
    }

    #[test]
    fn evict_expired() {
        let mut c: ExpiringLruCache<u8, ExpiredU8> =
            ExpiringLruCache::builder().max_size(3).build().unwrap();

        assert_eq!(c.set(1, 100), None);
        // The previous value 100 was expired (>10), so it is filtered from the return (None),
        // matching the TTL stores' cache_set contract.
        assert_eq!(c.set(1, 200), None);
        assert_eq!(c.set(2, 1), None);
        assert_eq!(c.cache_size(), 2);

        // It should only evict n > 10
        assert_eq!(2, c.cache_size());
        let _ = c.evict();
        assert_eq!(1, c.cache_size());
    }

    #[test]
    fn reset_rebuilds_store_and_preserves_on_evict() {
        let evicted = Arc::new(AtomicU64::new(0));
        let evicted_for_callback = evicted.clone();
        let mut c: ExpiringLruCache<u8, ExpiredU8> = ExpiringLruCache::builder()
            .max_size(1)
            .on_evict(move |_key: &u8, _value: &ExpiredU8| {
                evicted_for_callback.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();

        c.set(1, 1);
        c.cache_reset();
        assert_eq!(0, c.cache_size());

        // Inserting two values into a capacity-1 cache should evict exactly one.
        c.set(2, 2);
        c.set(3, 3);
        assert_eq!(1, evicted.load(Ordering::Relaxed));

        // Insert a third value — eviction count should now be exactly 2, not more.
        c.set(4, 4);
        assert_eq!(2, evicted.load(Ordering::Relaxed));
    }

    #[test]
    fn cache_get_with_expiry_status_does_not_promote_expired_entry() {
        // Build a capacity-2 cache. Insert A then B, making B the MRU entry.
        let mut c: ExpiringLruCache<u8, ExpiredU8> =
            ExpiringLruCache::builder().max_size(2).build().unwrap();
        c.set(1, 100); // A — value 100 > 10, so it is expired
        c.set(2, 100); // B — also expired

        // Calling cache_get_with_expiry_status on A must NOT promote A to MRU.
        let (val, expired) = c.cache_get_with_expiry_status(&1u8);
        assert!(val.is_some(), "expired entry should still be returned");
        assert!(expired, "entry should be flagged as expired");

        // Now insert a third key C to force a capacity eviction.
        // If A was wrongly promoted it would be MRU and B would be evicted instead.
        // Correct behaviour: B is still MRU → A (LRU) is evicted first.
        c.set(3, 1); // C — value 1 <= 10, live
        assert_eq!(c.cache_size(), 2);
        // A should have been evicted (LRU), B and C should still be present.
        assert!(
            c.get(&1u8).is_none(),
            "key 1 (A) should have been evicted as LRU"
        );
        assert!(
            c.get(&2u8).is_none(),
            "key 2 (B) is expired — none after get"
        );
        assert!(c.get(&3u8).is_some(), "key 3 (C) should be live");
    }

    #[test]
    fn cache_clear_with_on_evict_fires_for_all_entries() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering as AOrdering};
        let count = Arc::new(AtomicUsize::new(0));
        let count2 = count.clone();
        let mut c: ExpiringLruCache<u8, ExpiredU8> = ExpiringLruCache::builder()
            .max_size(5)
            .on_evict(move |_k: &u8, _v: &ExpiredU8| {
                count2.fetch_add(1, AOrdering::Relaxed);
            })
            .build()
            .unwrap();
        c.cache_set(1, 5); // live (value <= 10)
        c.cache_set(2, 12); // expired (value > 10)
        c.cache_set(3, 8); // live
        c.cache_clear_with_on_evict();
        assert_eq!(c.cache_size(), 0);
        assert_eq!(
            count.load(AOrdering::Relaxed),
            3,
            "on_evict fires for all entries including expired"
        );
        assert_eq!(c.evictions.load(AOrdering::Relaxed), 3);
    }

    #[test]
    fn cache_clear_does_not_fire_on_evict() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering as AOrdering};
        let count = Arc::new(AtomicUsize::new(0));
        let count2 = count.clone();
        let mut c: ExpiringLruCache<u8, ExpiredU8> = ExpiringLruCache::builder()
            .max_size(5)
            .on_evict(move |_k: &u8, _v: &ExpiredU8| {
                count2.fetch_add(1, AOrdering::Relaxed);
            })
            .build()
            .unwrap();
        c.cache_set(1, 5);
        c.cache_set(2, 8);
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
        let mut c: ExpiringLruCache<u8, ExpiredU8> = ExpiringLruCache::builder()
            .max_size(4)
            .on_evict(move |_k, _v| {
                evict_count2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();
        c.cache_set(1, 5);
        c.cache_set(2, 5);
        c.cache_set(3, 5);
        c.cache_reset();
        assert_eq!(
            evict_count.load(Ordering::Relaxed),
            0,
            "cache_reset must not fire on_evict"
        );
        assert_eq!(c.cache_size(), 0);
    }

    #[test]
    fn test_expiring_value_cache_iter_excludes_expired() {
        let mut c: ExpiringLruCache<u8, ExpiredU8> =
            ExpiringLruCache::builder().max_size(3).build().unwrap();
        c.cache_set(1, 5); // live
        c.cache_set(2, 12); // expired (value > 10)
        c.cache_set(3, 8); // live

        let mut keys: Vec<u8> = c.iter().map(|(&k, _)| k).collect();
        keys.sort();
        assert_eq!(keys, vec![1, 3]);
    }

    #[test]
    fn test_expiring_value_cache_clone() {
        let mut c: ExpiringLruCache<u8, ExpiredU8> =
            ExpiringLruCache::builder().max_size(3).build().unwrap();
        c.cache_set(1, 5);
        c.cache_set(2, 6);

        let mut cloned = c.clone();
        assert_eq!(cloned.cache_size(), 2);
        assert_eq!(cloned.cache_get(&1), Some(&5));
        assert_eq!(cloned.cache_get(&2), Some(&6));
    }

    #[test]
    fn test_expiring_value_cache_debug() {
        let c: ExpiringLruCache<u8, ExpiredU8> =
            ExpiringLruCache::builder().max_size(3).build().unwrap();
        let debug_str = format!("{:?}", c);
        assert!(debug_str.contains("ExpiringLruCache"));
        assert!(debug_str.contains("hits"));
        assert!(debug_str.contains("misses"));
        assert!(debug_str.contains("evictions"));
    }

    #[test]
    fn test_expiring_value_cache_remove_and_clear() {
        let mut c: ExpiringLruCache<u8, ExpiredU8> =
            ExpiringLruCache::builder().max_size(3).build().unwrap();
        c.cache_set(1, 5);
        c.cache_set(2, 6);

        assert_eq!(c.cache_remove(&1), Some(5));
        assert_eq!(c.cache_size(), 1);
        assert_eq!(c.cache_get(&1), None);

        c.cache_clear();
        assert_eq!(c.cache_size(), 0);
    }

    #[test]
    fn cache_remove_entry_returns_some_for_live_entry() {
        let mut c: ExpiringLruCache<u8, ExpiredU8> =
            ExpiringLruCache::builder().max_size(4).build().unwrap();
        c.cache_set(1, 5); // not expired: 5 <= 10
        let removed = c.cache_remove_entry(&1u8);
        assert_eq!(removed, Some((1u8, 5u8)));
        assert_eq!(c.cache_size(), 0);
    }

    #[test]
    fn cache_remove_entry_returns_some_for_expired_entry() {
        let mut c: ExpiringLruCache<u8, ExpiredU8> =
            ExpiringLruCache::builder().max_size(4).build().unwrap();
        c.cache_set(1, 20u8); // expired: 20 > 10

        // cache_remove returns None for an expired entry.
        c.cache_set(2, 20u8);
        assert_eq!(c.cache_remove(&2u8), None); // expired

        // cache_remove_entry returns Some even for an expired entry.
        let removed = c.cache_remove_entry(&1u8);
        assert_eq!(
            removed.expect("cache_remove_entry must return Some for expired entry"),
            (1u8, 20u8)
        );
    }

    #[test]
    fn cache_delete_returns_true_for_expired_entry() {
        let mut c: ExpiringLruCache<u8, ExpiredU8> =
            ExpiringLruCache::builder().max_size(4).build().unwrap();
        c.cache_set(1, 20u8); // expired
        assert!(
            c.cache_delete(&1u8),
            "cache_delete must return true for expired entry"
        );
        assert!(!c.cache_delete(&1u8), "cache_delete false when absent");
    }

    #[test]
    fn cache_remove_entry_fires_on_evict_for_expired() {
        let count = std::sync::Arc::new(AtomicU64::new(0));
        let count2 = count.clone();
        let mut c = ExpiringLruCache::builder()
            .max_size(4)
            .on_evict(move |_k: &u8, _v: &ExpiredU8| {
                count2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();
        c.cache_set(1u8, 20u8); // expired

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
        let mut c: ExpiringLruCache<u8, ExpiredU8> =
            ExpiringLruCache::builder().max_size(4).build().unwrap();
        assert_eq!(c.cache_remove_entry(&42u8), None);
    }

    #[test]
    fn cache_remove_entry_increments_eviction_counter() {
        let mut c: ExpiringLruCache<u8, ExpiredU8> =
            ExpiringLruCache::builder().max_size(4).build().unwrap();
        c.cache_set(1u8, 20u8); // expired: 20 > 10
        let before = c.cache_evictions().expect("evictions are always tracked");
        let _ = c.cache_remove_entry(&1u8); // expired but present - must increment
        let _ = c.cache_remove_entry(&99u8); // absent - must not increment
        assert_eq!(
            c.cache_evictions().expect("evictions are always tracked") - before,
            1,
            "cache_remove_entry must increment evictions for present key only"
        );
    }

    #[test]
    fn set_max_size_changes_capacity_and_evicts() {
        let mut c: ExpiringLruCache<u8, ExpiredU8> =
            ExpiringLruCache::builder().max_size(3).build().unwrap();
        c.cache_set(1, 1);
        c.cache_set(2, 2);
        c.cache_set(3, 3);
        assert_eq!(c.capacity(), 3);

        // Shrink to 2: LRU entry (1) should be evicted.
        let prev = c.set_max_size(2);
        assert_eq!(prev, Some(3));
        assert_eq!(c.capacity(), 2);
        assert_eq!(c.cache_size(), 2);

        // Insert beyond new cap triggers eviction.
        c.cache_set(4, 4);
        assert_eq!(c.cache_size(), 2);
    }

    #[test]
    fn set_max_size_shrink_fires_on_evict_and_counts_evictions() {
        use std::sync::{Arc, Mutex};
        let evicted_keys: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let evicted_keys2 = evicted_keys.clone();
        let mut c: ExpiringLruCache<u8, ExpiredU8> = ExpiringLruCache::builder()
            .max_size(4)
            .on_evict(move |k: &u8, _v: &ExpiredU8| {
                evicted_keys2.lock().unwrap().push(*k);
            })
            .build()
            .unwrap();

        // Values 1..=4 are all <= 10, so none are expired.
        c.cache_set(1, 1);
        c.cache_set(2, 2);
        c.cache_set(3, 3);
        c.cache_set(4, 4);
        // Touch 1 and 2 so 3 and 4 become least-recently-used.
        assert_eq!(c.cache_get(&1), Some(&1));
        assert_eq!(c.cache_get(&2), Some(&2));

        let evictions_before = c.cache_evictions().expect("evictions tracked");
        let prev = c.set_max_size(2);
        assert_eq!(prev, Some(4));
        assert_eq!(c.capacity(), 2);
        assert_eq!(c.cache_size(), 2);

        // Two entries were dropped; eviction counter must reflect that.
        assert_eq!(
            c.cache_evictions().expect("evictions tracked") - evictions_before,
            2,
            "set_max_size shrink must increment cache_evictions by the number of dropped entries"
        );

        // on_evict must have fired for exactly the two LRU keys (3 and 4).
        let mut fired: Vec<u8> = evicted_keys.lock().unwrap().clone();
        fired.sort();
        assert_eq!(
            fired,
            vec![3, 4],
            "on_evict must fire for the evicted (least-recently-used) keys"
        );

        // The two most-recently-used entries must survive.
        assert_eq!(c.cache_get(&1), Some(&1));
        assert_eq!(c.cache_get(&2), Some(&2));
        assert_eq!(c.cache_get(&3), None);
        assert_eq!(c.cache_get(&4), None);
    }

    #[test]
    fn try_set_max_size_rejects_zero() {
        let mut c: ExpiringLruCache<u8, ExpiredU8> =
            ExpiringLruCache::builder().max_size(3).build().unwrap();
        assert_eq!(
            c.try_set_max_size(0),
            Err(super::super::SetMaxSizeError::ZeroSize)
        );
        assert_eq!(c.try_set_max_size(5).unwrap(), Some(3));
    }

    #[test]
    #[should_panic(expected = "max_size must be greater than zero")]
    fn set_max_size_zero_panics() {
        let mut c: ExpiringLruCache<u8, ExpiredU8> =
            ExpiringLruCache::builder().max_size(3).build().unwrap();
        c.set_max_size(0);
    }

    #[test]
    fn eq_same_entries_compare_equal() {
        let mut a: ExpiringLruCache<u8, ExpiredU8> =
            ExpiringLruCache::builder().max_size(4).build().unwrap();
        let mut b: ExpiringLruCache<u8, ExpiredU8> =
            ExpiringLruCache::builder().max_size(4).build().unwrap();
        a.cache_set(1, 5);
        a.cache_set(2, 6);
        // Insert in a different order: inner LruCache equality is membership-based.
        b.cache_set(2, 6);
        b.cache_set(1, 5);
        assert_eq!(
            a, b,
            "caches with the same stored entries must compare equal"
        );
    }

    #[test]
    fn eq_ignores_metrics_and_on_evict() {
        // Equality is over stored entries only: differing metrics and an
        // `on_evict` callback on one side must not break it.
        let mut a: ExpiringLruCache<u8, ExpiredU8> =
            ExpiringLruCache::builder().max_size(4).build().unwrap();
        let mut b: ExpiringLruCache<u8, ExpiredU8> = ExpiringLruCache::builder()
            .max_size(4)
            .on_evict(|_k: &u8, _v: &ExpiredU8| {})
            .build()
            .unwrap();
        a.cache_set(1, 5);
        b.cache_set(1, 5);
        // Drive `a`'s metrics away from `b`'s.
        a.cache_get(&1);
        a.cache_get(&99);
        assert_ne!(a.cache_hits(), b.cache_hits());
        assert_eq!(
            a, b,
            "metrics and on_evict must not participate in equality"
        );
    }

    #[test]
    fn ne_differing_entries() {
        let mut a: ExpiringLruCache<u8, ExpiredU8> =
            ExpiringLruCache::builder().max_size(4).build().unwrap();
        let mut b: ExpiringLruCache<u8, ExpiredU8> =
            ExpiringLruCache::builder().max_size(4).build().unwrap();
        a.cache_set(1, 5);
        b.cache_set(1, 6); // same key, different value
        assert_ne!(a, b, "differing values must compare unequal");

        let mut c: ExpiringLruCache<u8, ExpiredU8> =
            ExpiringLruCache::builder().max_size(4).build().unwrap();
        c.cache_set(1, 5);
        c.cache_set(2, 5); // extra key
        assert_ne!(a, c, "differing key sets must compare unequal");

        // An empty cache differs from a populated one and equals another empty one.
        let empty1: ExpiringLruCache<u8, ExpiredU8> =
            ExpiringLruCache::builder().max_size(4).build().unwrap();
        let empty2: ExpiringLruCache<u8, ExpiredU8> =
            ExpiringLruCache::builder().max_size(4).build().unwrap();
        assert_eq!(empty1, empty2);
        assert_ne!(empty1, a);
    }

    // --- expires_at tests ---

    /// A type that overrides `expires_at` to return a concrete deadline.
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

    #[test]
    fn expires_at_default_returns_none() {
        // ExpiredU8 does not override expires_at, so the default must return None.
        let v: ExpiredU8 = 5;
        assert_eq!(
            v.expires_at(),
            None,
            "default expires_at must return None for types that do not track a deadline"
        );
    }

    #[test]
    fn expires_at_override_returns_some_instant() {
        let deadline = crate::time::Instant::now() + std::time::Duration::from_secs(60);
        let v = TimedValue { deadline };
        assert_eq!(
            v.expires_at(),
            Some(deadline),
            "expires_at must return the overridden deadline when the impl provides one"
        );
        // Confirm is_expired is not confused: a future deadline is not yet expired.
        assert!(
            !v.is_expired(),
            "a value whose deadline is in the future must not be expired"
        );
    }

    /// `is_expired` is the authoritative liveness check; `expires_at` is advisory only.
    /// This type deliberately reports a deadline that is already in the past while
    /// claiming to be live. A correct cache must consult `is_expired` (live), NOT
    /// `expires_at` (past), and therefore keep the entry.
    struct LiveDespitePastDeadline {
        past: crate::time::Instant,
    }

    impl Expires for LiveDespitePastDeadline {
        fn is_expired(&self) -> bool {
            // Authoritative: always live, regardless of the advisory deadline below.
            false
        }

        fn expires_at(&self) -> Option<crate::time::Instant> {
            // Advisory: a deadline in the past. Must not be used for liveness.
            Some(self.past)
        }
    }

    #[test]
    fn expires_at_past_does_not_override_is_expired_for_value() {
        // Sanity at the value level: the two methods disagree on purpose.
        let v = LiveDespitePastDeadline {
            past: crate::time::Instant::now() - std::time::Duration::from_secs(3600),
        };
        assert!(
            !v.is_expired(),
            "is_expired is authoritative and reports the value as live"
        );
        let reported = v.expires_at().expect("override returns Some");
        assert!(
            reported < crate::time::Instant::now(),
            "expires_at advisory deadline is in the past"
        );
    }

    #[test]
    fn cache_keeps_entry_with_past_expires_at_but_live_is_expired() {
        // Contract: the cache must decide liveness from is_expired, not expires_at.
        // The stored value's expires_at is in the past, but is_expired() == false,
        // so the entry must be returned as a live hit and survive in the cache.
        let past = crate::time::Instant::now() - std::time::Duration::from_secs(3600);
        let mut c: ExpiringLruCache<u8, LiveDespitePastDeadline> =
            ExpiringLruCache::builder().max_size(3).build().unwrap();
        c.cache_set(1, LiveDespitePastDeadline { past });

        // get must treat it as a live hit (is_expired() == false).
        assert!(
            c.cache_get(&1u8).is_some(),
            "entry whose is_expired() is false must be returned even if expires_at is in the past"
        );
        assert_eq!(c.cache_hits(), Some(1), "the access must count as a hit");
        assert_eq!(
            c.cache_misses(),
            Some(0),
            "an entry the cache treats as live must not register a miss"
        );
        assert_eq!(c.cache_size(), 1, "the live entry must remain in the cache");

        // evict() must also consult is_expired, not expires_at: nothing is removed.
        assert_eq!(
            c.evict(),
            0,
            "evict must not remove an entry whose is_expired() is false"
        );
        assert_eq!(c.cache_size(), 1);

        // peek and iter must likewise keep it.
        assert!(
            c.cache_peek(&1u8).is_some(),
            "peek must surface the live entry"
        );
        let keys: Vec<u8> = c.iter().map(|(&k, _)| k).collect();
        assert_eq!(keys, vec![1], "iter must include the live entry");
    }

    /// A type that provides ONLY `is_expired`, relying on the trait default for
    /// `expires_at`. The fact that this compiles and is usable as a cache value is
    /// the contract: adding `expires_at` did not break impls that omit it.
    struct OnlyIsExpired(bool);

    impl Expires for OnlyIsExpired {
        fn is_expired(&self) -> bool {
            self.0
        }
        // expires_at intentionally not provided — exercises the default impl.
    }

    #[test]
    fn impl_with_only_is_expired_compiles_and_defaults_expires_at_to_none() {
        let live = OnlyIsExpired(false);
        assert!(!live.is_expired());
        assert_eq!(
            live.expires_at(),
            None,
            "an impl omitting expires_at must inherit the None default"
        );

        // And it works as a cache value type end to end.
        let mut c: ExpiringLruCache<u8, OnlyIsExpired> =
            ExpiringLruCache::builder().max_size(2).build().unwrap();
        c.cache_set(1, OnlyIsExpired(false)); // live
        c.cache_set(2, OnlyIsExpired(true)); // expired
        assert!(c.cache_get(&1u8).is_some(), "live entry returned");
        assert!(c.cache_get(&2u8).is_none(), "expired entry not returned");
    }
}
