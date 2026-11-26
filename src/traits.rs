//! The cache traits. Defined here and re-exported at the crate root.

use super::*;

/// Core cache operations for single-owner (non-concurrent) stores.
///
/// The `cache_`-prefixed methods are the required core: stores implement only these.
/// Short aliases (`get`/`set`/`remove`/`clear`/`len`/...) are provided by the blanket
/// extension trait [`CachedExt`], which is automatically implemented for any type that
/// implements `Cached`. Bring the short names into scope with
/// `use cached::prelude::*;` or `use cached::CachedExt;`.
///
/// **Not dyn-compatible.** `Cached` has generic required methods (the `_mut`
/// get-or-set family), so `dyn Cached<K, V>` does not work; use a concrete store
/// type. For a trait-object-friendly cache use the internally-synchronized
/// [`ConcurrentCached`] family instead, which keeps its one generic method out of
/// the vtable (`dyn ConcurrentCached<K, V, Error = E>` — the `Error` associated
/// type comes from the `ConcurrentCacheBase` supertrait and is specified inline).
///
/// The async traits (`CachedGetOrSetAsync`, `ConcurrentCachedAsync`) use the `async_cache_*` spelling
/// (`async_cache_get`, `async_cache_set`, `async_cache_remove`, ...) so they never collide
/// with these synchronous methods. The async traits have no short aliases.
///
/// Via `CachedExt` (short names):
///
/// ```rust
/// use cached::{CachedExt, UnboundCache};
///
/// let mut cache: UnboundCache<String, String> = UnboundCache::builder().build().unwrap();
/// cache.set("key".to_string(), "owned value".to_string());
/// let v = cache.get("key");
/// assert_eq!(v, Some(&"owned value".to_string()));
/// ```
///
/// Via `Cached` directly (`cache_*` prefix, use when only `Cached` is in scope):
///
/// ```rust
/// use cached::{Cached, UnboundCache};
///
/// let mut cache: UnboundCache<String, String> = UnboundCache::builder().build().unwrap();
/// cache.cache_set("key".to_string(), "owned value".to_string());
/// let v = cache.cache_get("key");
/// assert_eq!(v, Some(&"owned value".to_string()));
/// ```
pub trait Cached<K, V> {
    // ── Associated types ──────────────────────────────────────────────────

    /// The error type returned by [`cache_try_set`](Cached::cache_try_set).
    ///
    /// Use [`std::convert::Infallible`] for stores where insertion can never fail.
    /// Every built-in in-memory store (including the TTL stores, which treat an
    /// `Instant`-overflowing expiry as never-expires) is infallible; the type exists
    /// for custom stores whose insertion can fail.
    ///
    /// The bound `std::error::Error + Send + Sync + 'static` lets generic code call
    /// `.unwrap()` on `Result<_, Self::Error>`, propagate with `?` into
    /// `Box<dyn std::error::Error>`, and send/share the error across threads without
    /// additional where-clauses at every use site.
    type Error: std::error::Error + Send + Sync + 'static;

    // ── Core required methods (stores implement these) ────────────────────

    /// Attempt to retrieve a cached value.
    ///
    /// Takes `&mut self` because some stores update recency, expiration timestamps,
    /// or metrics during reads.
    ///
    /// ```rust
    /// # use cached::{CachedExt, UnboundCache};
    /// # let mut cache: UnboundCache<String, String> = UnboundCache::builder().build().unwrap();
    /// # cache.set("key".to_string(), "owned value".to_string());
    /// let v1 = cache.get("key").map(String::clone);
    /// let v2 = cache.get(&"key".to_string()).map(String::clone);
    /// # assert_eq!(v1, v2);
    /// ```
    fn cache_get<Q>(&mut self, k: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized;

    /// Attempt to retrieve a cached value with mutable access.
    fn cache_get_mut<Q>(&mut self, k: &Q) -> Option<&mut V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized;

    /// Insert a key-value pair and return the previous value. On recency-ordered stores,
    /// overwriting an existing key promotes it to most-recently-used.
    fn cache_set(&mut self, k: K, v: V) -> Option<V>;

    /// Fallible variant of [`Self::cache_set`] for custom stores whose insertion can fail.
    /// The default implementation is infallible and delegates to [`Self::cache_set`]; it
    /// always returns `Ok`. Every built-in in-memory store keeps that default
    /// (`type Error = std::convert::Infallible`).
    ///
    /// Generic code that must support fallible custom stores should call this method
    /// instead of [`cache_set`](Self::cache_set) and propagate [`Self::Error`](Cached::Error);
    /// for infallible stores the two are equivalent.
    fn cache_try_set(&mut self, k: K, v: V) -> Result<Option<V>, Self::Error> {
        Ok(self.cache_set(k, v))
    }

    /// Get or insert a key-value pair, returning a mutable reference to the value.
    ///
    /// This is the mutable counterpart of [`cache_get_or_set_with`](Cached::cache_get_or_set_with).
    /// Stores implement this method; the shared-reference variant delegates to it.
    fn cache_get_or_set_with_mut<F: FnOnce() -> V>(&mut self, key: K, f: F) -> &mut V;

    /// Get or insert a key-value pair, propagating errors from the factory and
    /// returning a mutable reference to the value.
    ///
    /// This is the mutable counterpart of
    /// [`cache_try_get_or_set_with`](Cached::cache_try_get_or_set_with).
    fn cache_try_get_or_set_with_mut<F: FnOnce() -> Result<V, E>, E>(
        &mut self,
        key: K,
        f: F,
    ) -> Result<&mut V, E>;

    /// Get or insert a key-value pair, returning a shared reference to the value.
    ///
    /// Returns `&V`. Use [`cache_get_or_set_with_mut`](Cached::cache_get_or_set_with_mut)
    /// when you need a mutable reference to the cached value. This is a provided default
    /// (it delegates to `cache_get_or_set_with_mut`); external stores implement `_mut`, not this.
    fn cache_get_or_set_with<F: FnOnce() -> V>(&mut self, key: K, f: F) -> &V {
        &*self.cache_get_or_set_with_mut(key, f)
    }

    /// Get or insert a key-value pair, propagating errors from the factory and
    /// returning a shared reference to the value.
    ///
    /// Returns `Result<&V, E>`. Use
    /// [`cache_try_get_or_set_with_mut`](Cached::cache_try_get_or_set_with_mut)
    /// when you need a mutable reference to the cached value. This is a provided default
    /// (it delegates to `cache_try_get_or_set_with_mut`); external stores implement `_mut`, not this.
    fn cache_try_get_or_set_with<F: FnOnce() -> Result<V, E>, E>(
        &mut self,
        key: K,
        f: F,
    ) -> Result<&V, E> {
        self.cache_try_get_or_set_with_mut(key, f).map(|v| &*v)
    }

    /// Remove a cached value, returning it if it was both present and still live.
    ///
    /// Removing any present entry fires the store's `on_evict` callback (if set) and,
    /// for stores that track evictions, increments the `evictions` metric consistent
    /// with automatic eviction. For stores with expiry, an entry that is present but
    /// already expired is still removed (and still fires `on_evict` / counts as an
    /// eviction when the store tracks evictions), but `None` is returned because the
    /// value is no longer valid.
    ///
    /// Use [`cache_remove_entry`](Cached::cache_remove_entry) when you need to
    /// distinguish "key absent" from "key present but expired", or when you need
    /// the stored key back (relevant when `K`'s `Eq` ignores some fields).
    ///
    /// ```rust
    /// # use cached::{CachedExt, UnboundCache};
    /// # let mut cache: UnboundCache<String, String> = UnboundCache::builder().build().unwrap();
    /// # cache.set("k1".to_string(), "v1".to_string());
    /// # cache.set("k2".to_string(), "v2".to_string());
    /// let r1 = cache.remove("k1");
    /// let r2 = cache.remove(&"k2".to_string());
    /// # assert_eq!(r1, Some("v1".to_string()));
    /// # assert_eq!(r2, Some("v2".to_string()));
    /// ```
    #[must_use]
    fn cache_remove<Q>(&mut self, k: &Q) -> Option<V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized;

    /// Remove a cached entry, returning the stored key and value whenever an entry
    /// was physically deleted — including entries that were present but already expired.
    ///
    /// This is the key difference from [`cache_remove`](Cached::cache_remove):
    /// - `cache_remove` returns `None` for both "key absent" and "key present but expired"
    /// - `cache_remove_entry` returns `Some((stored_key, value))` whenever anything was
    ///   physically deleted, and `None` only when the key was not in the store at all
    ///
    /// This lets callers distinguish between the two cases, and also returns the *stored*
    /// key rather than the lookup key — relevant when `K`'s `Eq`/`Hash` ignores some
    /// fields and the stored and lookup instances may differ.
    ///
    /// Removing any present entry fires the store's `on_evict` callback (if set) and,
    /// for stores that track evictions, increments the `evictions` metric.
    ///
    /// ```rust
    /// use cached::{Cached, UnboundCache};
    ///
    /// let mut cache: UnboundCache<String, u32> = UnboundCache::builder().build().unwrap();
    /// cache.cache_set("key".to_string(), 42);
    ///
    /// // cache_remove_entry returns Some even for the key that was just inserted.
    /// let entry = cache.cache_remove_entry("key");
    /// assert_eq!(entry, Some(("key".to_string(), 42)));
    ///
    /// // Returns None only when the key was never present.
    /// assert_eq!(cache.cache_remove_entry("missing"), None);
    /// ```
    #[must_use]
    fn cache_remove_entry<Q>(&mut self, k: &Q) -> Option<(K, V)>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized;

    /// Remove all cached entries but preserve capacity allocation and metrics.
    /// To also reset metrics, call [`cache_reset_metrics`](Cached::cache_reset_metrics) afterward,
    /// or use [`cache_reset`](Cached::cache_reset) to do both at once.
    fn cache_clear(&mut self);

    /// Reset all entries and metrics (hits, misses, evictions) to zero.
    /// Store configuration — capacity, TTL, and `on_evict` callbacks — is preserved.
    /// To reset entries without resetting metrics, use [`cache_clear`](Cached::cache_clear).
    fn cache_reset(&mut self);

    /// Return the number of entries currently in the cache.
    ///
    /// This is a cheap read of the stored entry count: no expiry scan is performed.
    /// On lazy-eviction stores (`TtlCache`, `LruTtlCache`, `TtlSortedCache`,
    /// `ExpiringCache`, `ExpiringLruCache`, and their sharded equivalents), the count
    /// may include entries that have expired but not yet been swept. As a result,
    /// `cache_size()` (and the `len()` alias) can exceed `iter().count()`.
    /// Call `evict()` first if you need an accurate count of live entries.
    #[must_use]
    fn cache_size(&self) -> usize;

    // ── Optional overrides ────────────────────────────────────────────────

    /// Reset hit/miss counters.
    fn cache_reset_metrics(&mut self) {}

    /// Return `true` if the key is present (and unexpired), `false` otherwise.
    ///
    /// The default delegates to [`cache_get`](Cached::cache_get) and therefore counts a hit or
    /// miss and may update recency/expiry state. All built-in single-owner stores override it
    /// peek-based: no hit/miss metrics, no LRU promotion, no TTL refresh, and expired entries
    /// report `false`. For a non-mutating borrow-returning lookup, use
    /// [`CachedPeek::cache_peek`].
    fn cache_contains<Q>(&mut self, k: &Q) -> bool
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        self.cache_get(k).is_some()
    }

    /// Return the number of times a cached value was successfully retrieved.
    #[must_use]
    fn cache_hits(&self) -> Option<u64> {
        None
    }

    /// Return the number of times a cached value was not found.
    #[must_use]
    fn cache_misses(&self) -> Option<u64> {
        None
    }

    /// Return the cache capacity, if bounded.
    ///
    /// "Capacity" here means the eviction bound — the configured `max_size` — not pre-allocated
    /// storage like `Vec::capacity`. Unbounded stores return `None`.
    #[must_use]
    fn cache_capacity(&self) -> Option<usize> {
        None
    }

    /// Delete a cached entry without returning it. Returns `true` if an entry was
    /// physically deleted (including expired entries), `false` if the key was absent.
    ///
    /// Unlike [`cache_remove`](Cached::cache_remove), this returns `true` even when the
    /// deleted entry was already expired.
    fn cache_delete<Q>(&mut self, k: &Q) -> bool
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        self.cache_remove_entry(k).is_some()
    }

    /// Return the number of times a value was evicted from the cache.
    #[must_use]
    fn cache_evictions(&self) -> Option<u64> {
        None
    }
}

/// Short-alias extension for [`Cached`] stores.
///
/// Every type that implements `Cached<K, V>` automatically implements this trait through a blanket
/// impl. The methods here are ergonomic short names that delegate to the `cache_`-prefixed core
/// methods on `Cached`. Import this trait (or `use cached::prelude::*;`) to bring the short names
/// into scope:
///
/// ```rust
/// use cached::{CachedExt, UnboundCache};
///
/// let mut cache: UnboundCache<String, u32> = UnboundCache::builder().build().unwrap();
/// cache.set("key".to_string(), 42);
/// assert_eq!(cache.get("key"), Some(&42u32));
/// assert!(cache.delete("key"));
/// assert!(!cache.delete("key"));
/// ```
///
/// When you need a short name that would collide with another trait's method, or when
/// only `Cached` is in scope, use the `cache_`-prefixed form:
///
/// ```rust
/// use cached::{Cached, UnboundCache};
///
/// let mut cache: UnboundCache<String, u32> = UnboundCache::builder().build().unwrap();
/// cache.cache_set("key".to_string(), 42);
/// assert_eq!(cache.cache_get("key"), Some(&42u32));
/// ```
///
/// Custom store implementations do **not** implement this trait directly. Only implement the
/// core `Cached` trait; this extension is provided automatically via the blanket impl.
///
/// Note: `shards()` and `shard_sizes()` on the sharded stores are inherent-only - they are
/// sharding-specific introspection not part of the generic cache trait surface.
pub trait CachedExt<K, V>: Cached<K, V> {
    /// Retrieve a cached value. Delegates to [`cache_get`](Cached::cache_get).
    ///
    /// # Mutability
    ///
    /// Like [`cache_get`](Cached::cache_get), this takes `&mut self` (unlike
    /// [`HashMap::get`](std::collections::HashMap::get)) because some stores update recency
    /// or refresh TTL on read. For a `&self` read where the store supports it, use
    /// [`CachedPeek::cache_peek`] (non-mutating, no recency/TTL/metrics updates) or
    /// [`CachedRead::cache_get_read`] (shared-lock read preserving normal read semantics).
    fn get<Q>(&mut self, k: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized;

    /// Retrieve a cached value with mutable access. Delegates to [`cache_get_mut`](Cached::cache_get_mut).
    fn get_mut<Q>(&mut self, k: &Q) -> Option<&mut V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized;

    /// Insert a key-value pair and return the previous value. Delegates to [`cache_set`](Cached::cache_set).
    fn set(&mut self, k: K, v: V) -> Option<V>;

    /// Fallible insert. Delegates to [`cache_try_set`](Cached::cache_try_set).
    fn try_set(&mut self, k: K, v: V) -> Result<Option<V>, Self::Error>;

    /// Get or insert a key-value pair. Delegates to [`cache_get_or_set_with`](Cached::cache_get_or_set_with).
    fn get_or_set_with<F: FnOnce() -> V>(&mut self, key: K, f: F) -> &V;

    /// Get or insert a key-value pair, returning a mutable reference. Delegates to
    /// [`cache_get_or_set_with_mut`](Cached::cache_get_or_set_with_mut).
    fn get_or_set_with_mut<F: FnOnce() -> V>(&mut self, key: K, f: F) -> &mut V;

    /// Get or insert a key-value pair with error handling. Delegates to
    /// [`cache_try_get_or_set_with`](Cached::cache_try_get_or_set_with).
    fn try_get_or_set_with<F: FnOnce() -> Result<V, E>, E>(&mut self, k: K, f: F) -> Result<&V, E>;

    /// Get or insert a key-value pair with error handling, returning a mutable reference.
    /// Delegates to [`cache_try_get_or_set_with_mut`](Cached::cache_try_get_or_set_with_mut).
    fn try_get_or_set_with_mut<F: FnOnce() -> Result<V, E>, E>(
        &mut self,
        k: K,
        f: F,
    ) -> Result<&mut V, E>;

    /// Remove a cached value. Delegates to [`cache_remove`](Cached::cache_remove).
    fn remove<Q>(&mut self, k: &Q) -> Option<V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized;

    /// Remove a cached entry, returning the stored key and value. Delegates to
    /// [`cache_remove_entry`](Cached::cache_remove_entry).
    fn remove_entry<Q>(&mut self, k: &Q) -> Option<(K, V)>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized;

    /// Delete a cached entry without returning it. Returns `true` if an entry was
    /// physically deleted (including expired entries). Delegates to
    /// [`cache_delete`](Cached::cache_delete).
    fn delete<Q>(&mut self, k: &Q) -> bool
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized;

    /// Return `true` if the cache contains a value for the given key.
    /// Delegates to [`cache_contains`](Cached::cache_contains).
    ///
    /// The receiver stays `&mut self` because a third-party store's implementation
    /// may mutate, but the built-in stores' overrides are peek-based and do not
    /// mutate observable state (no hit/miss count, no recency update, no TTL
    /// refresh). For a non-mutating borrow-returning lookup use
    /// [`CachedPeek::cache_peek`] if the store implements it.
    fn contains<Q>(&mut self, k: &Q) -> bool
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized;

    /// Remove all entries, keeping allocated memory for reuse. Delegates to
    /// [`cache_clear`](Cached::cache_clear).
    fn clear(&mut self);

    /// Remove all entries and reset metrics to zero. Delegates to
    /// [`cache_reset`](Cached::cache_reset).
    fn reset(&mut self);

    /// Return the number of entries currently in the cache. Delegates to
    /// [`cache_size`](Cached::cache_size).
    ///
    /// On lazy-eviction stores this count may include expired-but-not-yet-swept entries.
    /// Use `evict()` (via [`CacheEvict`]) before calling `len()` if
    /// you need an accurate count of live entries.
    #[must_use]
    fn len(&self) -> usize;

    /// Return `true` if the cache contains no entries. Delegates to
    /// [`cache_size`](Cached::cache_size).
    ///
    /// On lazy-eviction stores an expired-but-not-yet-swept entry causes this to return
    /// `false` even when no live entries remain.
    #[must_use]
    fn is_empty(&self) -> bool;

    /// Return the number of cache hits, if tracked. Delegates to
    /// [`cache_hits`](Cached::cache_hits).
    #[must_use]
    fn hits(&self) -> Option<u64>;

    /// Return the number of cache misses, if tracked. Delegates to
    /// [`cache_misses`](Cached::cache_misses).
    #[must_use]
    fn misses(&self) -> Option<u64>;

    /// Return the cache's capacity bound, if it has one. Delegates to
    /// [`cache_capacity`](Cached::cache_capacity).
    ///
    /// Note: the always-bounded LRU-family stores (`LruCache`, `LruTtlCache`,
    /// `ExpiringLruCache`) also expose an inherent `capacity` returning a plain
    /// `usize`, which takes call-site priority over this alias on those concrete
    /// types; the alias applies in generic `Cached`-bounded code.
    #[must_use]
    fn capacity(&self) -> Option<usize>;

    /// Return the number of evictions, if tracked. Delegates to
    /// [`cache_evictions`](Cached::cache_evictions).
    #[must_use]
    fn evictions(&self) -> Option<u64>;

    /// Return a snapshot of cache metrics.
    #[must_use]
    fn metrics(&self) -> CacheMetrics;
}

impl<K, V, T: Cached<K, V>> CachedExt<K, V> for T {
    fn get<Q>(&mut self, k: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        self.cache_get(k)
    }

    fn get_mut<Q>(&mut self, k: &Q) -> Option<&mut V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        self.cache_get_mut(k)
    }

    fn set(&mut self, k: K, v: V) -> Option<V> {
        self.cache_set(k, v)
    }

    fn try_set(&mut self, k: K, v: V) -> Result<Option<V>, Self::Error> {
        self.cache_try_set(k, v)
    }

    fn get_or_set_with<F: FnOnce() -> V>(&mut self, key: K, f: F) -> &V {
        self.cache_get_or_set_with(key, f)
    }

    fn get_or_set_with_mut<F: FnOnce() -> V>(&mut self, key: K, f: F) -> &mut V {
        self.cache_get_or_set_with_mut(key, f)
    }

    fn try_get_or_set_with<F: FnOnce() -> Result<V, E>, E>(&mut self, k: K, f: F) -> Result<&V, E> {
        self.cache_try_get_or_set_with(k, f)
    }

    fn try_get_or_set_with_mut<F: FnOnce() -> Result<V, E>, E>(
        &mut self,
        k: K,
        f: F,
    ) -> Result<&mut V, E> {
        self.cache_try_get_or_set_with_mut(k, f)
    }

    fn remove<Q>(&mut self, k: &Q) -> Option<V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        self.cache_remove(k)
    }

    fn remove_entry<Q>(&mut self, k: &Q) -> Option<(K, V)>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        self.cache_remove_entry(k)
    }

    fn delete<Q>(&mut self, k: &Q) -> bool
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        self.cache_delete(k)
    }

    fn contains<Q>(&mut self, k: &Q) -> bool
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        self.cache_contains(k)
    }

    fn clear(&mut self) {
        self.cache_clear()
    }

    fn reset(&mut self) {
        self.cache_reset()
    }

    fn len(&self) -> usize {
        self.cache_size()
    }

    fn is_empty(&self) -> bool {
        self.cache_size() == 0
    }

    fn hits(&self) -> Option<u64> {
        self.cache_hits()
    }

    fn misses(&self) -> Option<u64> {
        self.cache_misses()
    }

    fn capacity(&self) -> Option<usize> {
        self.cache_capacity()
    }

    fn evictions(&self) -> Option<u64> {
        self.cache_evictions()
    }

    fn metrics(&self) -> CacheMetrics {
        CacheMetrics {
            hits: self.cache_hits(),
            misses: self.cache_misses(),
            evictions: self.cache_evictions(),
            entry_count: Some(self.cache_size()),
            capacity: self.cache_capacity(),
        }
    }
}

/// Iteration over cache contents for stores that can expose borrowed entries.
///
/// **Contract for timed and expiring stores**: `iter()` (and `keys()`, `values()`) omit
/// expired entries from the yielded view but do **not** remove them from the store. The
/// receiver is `&self`, so no mutation occurs during iteration. As a result,
/// `iter().count()` may be less than `len()` when expired-but-not-yet-swept entries are
/// present. Call `evict()` (via [`CacheEvict`]) to physically remove
/// expired entries and reclaim memory.
///
/// Sharded stores (`Sharded*`) do not implement this trait; they are internally
/// synchronized and cannot expose borrowed references through `&self` iteration.
/// Use `ConcurrentCacheEvict::evict` on sharded stores to sweep expired entries.
pub trait CachedIter<K, V> {
    /// Return an iterator over the key-value pairs in the cache.
    ///
    /// On timed/expiring stores, expired entries are omitted from the iterator without
    /// being removed. The returned count may be less than `len()`.
    fn iter<'a>(&'a self) -> impl Iterator<Item = (&'a K, &'a V)> + 'a
    where
        Self: Sized,
        K: 'a,
        V: 'a;

    /// Return an iterator over the keys in the cache.
    fn keys<'a>(&'a self) -> impl Iterator<Item = &'a K> + 'a
    where
        Self: Sized,
        K: 'a,
        V: 'a,
    {
        self.iter().map(|(k, _)| k)
    }

    /// Return an iterator over the values in the cache.
    fn values<'a>(&'a self) -> impl Iterator<Item = &'a V> + 'a
    where
        Self: Sized,
        K: 'a,
        V: 'a,
    {
        self.iter().map(|(_, v)| v)
    }
}

/// A snapshot of cache hit/miss and size statistics.
///
/// This struct is `#[non_exhaustive]`: new metric fields may be added in future releases, so it
/// cannot be constructed with struct-literal syntax (including `..Default::default()`) or
/// destructured exhaustively from outside the crate. To build one (e.g. when overriding
/// `metrics()` in an external store), start from [`Default`] and assign the fields you track:
///
/// ```
/// use cached::CacheMetrics;
/// let mut m = CacheMetrics::default();
/// m.hits = Some(10);
/// m.misses = Some(2);
/// assert_eq!(m.hit_ratio(), Some(10.0 / 12.0));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct CacheMetrics {
    /// Number of successful cache lookups, if tracked.
    pub hits: Option<u64>,
    /// Number of failed cache lookups, if tracked.
    pub misses: Option<u64>,
    /// Number of entries evicted from the cache, if tracked.
    pub evictions: Option<u64>,
    /// Current number of entries in the cache.
    ///
    /// `None` means the count is unknown or unavailable for this store (e.g. redis/redb,
    /// whose `cache_size` returns `Ok(None)`; for redb, `RedbCache::scan_stats()` counts on
    /// demand); `Some(n)` is the exact live entry count.
    pub entry_count: Option<usize>,
    /// Maximum capacity, if bounded.
    pub capacity: Option<usize>,
}

impl CacheMetrics {
    /// Return the cache hit ratio as a value in `[0.0, 1.0]`, or `None` if no lookups have occurred.
    #[must_use]
    pub fn hit_ratio(&self) -> Option<f64> {
        let hits = self.hits?;
        let misses = self.misses?;
        let total = hits + misses;
        if total == 0 {
            None
        } else {
            Some(hits as f64 / total as f64)
        }
    }
}

/// Non-mutating cache lookup for stores that can expose a value by shared reference.
///
/// Peeking does not update recency, refresh TTLs, increment hit/miss metrics, or evict expired
/// values. It is useful for diagnostics and for implementing read APIs in stores whose normal
/// lookup semantics do not require recency or refresh mutation.
pub trait CachedPeek<K, V> {
    /// Attempt to retrieve a cached value without mutating the cache.
    fn cache_peek<Q>(&self, k: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized;

    /// Ergonomic alias for [`cache_peek`](Self::cache_peek).
    fn peek<Q>(&self, k: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        self.cache_peek(k)
    }
}

/// Shared-reference cache lookup for stores that can preserve normal read semantics without an
/// exclusive mutable borrow.
///
/// This trait is intentionally narrower than [`CachedPeek`], and the two are distinct on purpose:
///
/// - [`CachedPeek::cache_peek`] is a non-promoting, side-effect-free glance: it never updates
///   recency, metrics, or TTL. Every single-owner store implements it.
/// - [`CachedRead::cache_get_read`] is a *normal read* that merely happens to need only a shared
///   (`&self`) borrow. `CachedRead` is the compile-time gate for `#[cached(unsync_reads = true)]`:
///   with `unsync_reads`, the generated cache-hit path takes a shared read lock instead of an
///   exclusive one, which is sound only for a store whose read path performs no mutation. A store
///   with LRU recency updates or refresh-on-hit TTL therefore must NOT implement `CachedRead`
///   (that is why `LruCache`/`LruTtlCache`/`TtlCache` do not), and a custom `ty` store used with
///   `unsync_reads` must implement it. Its default forwards to `cache_peek`.
pub trait CachedRead<K, V>: CachedPeek<K, V> {
    /// Attempt to retrieve a cached value through a shared reference.
    fn cache_get_read<Q>(&self, k: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        self.cache_peek(k)
    }
}

/// Extra cache operations for types that implement `Clone`.
///
/// [`cache_get_with_expiry_status`](CloneCached::cache_get_with_expiry_status)
/// returns `(value, expired)` — note an expired entry is still *returned*
/// (`(Some(v), true)`) so callers can fall back to the stale value if a refresh
/// fails:
///
/// ```rust
/// # #[cfg(feature = "time_stores")]
/// # {
/// use cached::{CachedExt, CloneCached, TtlCache};
/// use cached::time::Duration;
///
/// let mut c = TtlCache::builder().ttl(Duration::from_secs(60)).build().unwrap();
/// c.set("k".to_string(), 1);
/// assert_eq!(c.get_with_expiry_status(&"k".to_string()), (Some(1), false)); // live
/// assert_eq!(c.get_with_expiry_status(&"x".to_string()), (None, false));    // absent
/// // (a present-but-expired entry would yield `(Some(v), true)`)
/// # }
/// ```
pub trait CloneCached<K, V> {
    /// Look up a cached value and report whether the found entry is expired.
    ///
    /// Returns `(value, expired)` where:
    /// - `(None, false)` — key not present (misses counter incremented)
    /// - `(Some(v), false)` — key present and live (hits counter incremented)
    /// - `(Some(v), true)` — key present but expired; the stale value is returned so callers
    ///   can fall back to it if a refresh fails. The entry is **not** removed from the cache
    ///   and eviction counters are **not** incremented (misses counter incremented).
    ///
    /// Unlike [`Cached::cache_get`], this never removes an expired entry — it intentionally
    /// leaves it in place so it can be returned as a stale fallback. Expired entries are swept
    /// by a subsequent `cache_get`, an explicit `cache_remove`, or `evict()`.
    fn cache_get_with_expiry_status<Q>(&mut self, key: &Q) -> (Option<V>, bool)
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
        V: Clone;

    /// Ergonomic alias for [`cache_get_with_expiry_status`](Self::cache_get_with_expiry_status).
    fn get_with_expiry_status<Q>(&mut self, key: &Q) -> (Option<V>, bool)
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
        V: Clone,
    {
        self.cache_get_with_expiry_status(key)
    }

    /// Non-renewing peek that also reports whether the found entry is expired.
    ///
    /// This is a required method. Implementations must satisfy the contract: same
    /// `(value, expired)` return shape as
    /// [`cache_get_with_expiry_status`](Self::cache_get_with_expiry_status), but the read
    /// must not produce any observable side effects: no LRU promotion, no hit/miss counter
    /// increment, no TTL renewal.
    ///
    /// Returns `(Some(v), true)` for a present-but-expired entry, `(None, false)` for an
    /// absent key, `(Some(v), false)` for a live entry.
    ///
    /// This is used on the `force_refresh` bypass path of `#[cached(result_fallback = true)]`
    /// to capture the stale fallback value without touching recency or metrics.
    fn cache_peek_with_expiry_status<Q>(&self, key: &Q) -> (Option<V>, bool)
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
        V: Clone;

    /// Ergonomic alias for [`cache_peek_with_expiry_status`](Self::cache_peek_with_expiry_status).
    fn peek_with_expiry_status<Q>(&self, key: &Q) -> (Option<V>, bool)
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
        V: Clone,
    {
        self.cache_peek_with_expiry_status(key)
    }
}

/// Side-effect-free per-key expiry read on the single-owner stores.
///
/// Two reads, both free of read side effects:
/// [`cache_peek_expires_at`](Self::cache_peek_expires_at) returns the stored value together with
/// the instant it expires at, and [`cache_expires_at`](Self::cache_expires_at) returns only
/// whether the key is present plus that instant, with no value, no clone, and no `V: Clone`
/// bound. Use the value-free one for a threshold check that needs the deadline and nothing else.
///
/// The trait configures nothing: despite sitting next to [`CacheTtl`] and [`CacheRefreshOnHit`],
/// both methods are reads.
///
/// [`cache_peek_with_expiry_status`](CloneCached::cache_peek_with_expiry_status) answers only
/// "expired: yes or no". This trait returns the deadline itself, so a caller can express a
/// "remaining TTL below a threshold" predicate and refresh early:
///
/// ```rust
/// # #[cfg(feature = "time_stores")]
/// # {
/// use cached::{CacheExpiry, CachedExt, TtlCache};
/// use cached::time::{Duration, Instant};
///
/// let mut c = TtlCache::builder().ttl(Duration::from_secs(60)).build().unwrap();
/// c.set("k".to_string(), 1);
///
/// // The key is a `Borrow<Q>` form, so a `String`-keyed cache takes a `&str` here.
/// let (value, expires_at) = c.peek_expires_at("k");
/// assert_eq!(value, Some(1));
///
/// let deadline = expires_at.expect("a configured ttl records a deadline");
/// let remaining = deadline.saturating_duration_since(Instant::now());
/// assert!(remaining > Duration::from_secs(50));
/// // the policy: refresh once less than 10s of the 60s ttl is left
/// let needs_refresh = remaining < Duration::from_secs(10);
/// assert!(!needs_refresh);
///
/// assert_eq!(c.peek_expires_at("absent"), (None, None));
///
/// // The same policy without the value: `(present, deadline)`, no clone.
/// let (present, expires_at) = c.expires_at("k");
/// assert!(present);
/// let deadline = expires_at.expect("a configured ttl records a deadline");
/// assert!(deadline.saturating_duration_since(Instant::now()) > Duration::from_secs(50));
/// assert!(deadline > Instant::now());
///
/// // Absent and never-expires are distinct, and call for opposite actions.
/// assert_eq!(c.expires_at("absent"), (false, None)); // fetch it
/// # }
/// ```
///
/// See [`examples/refresh_before_expiry.rs`](https://github.com/jaemk/cached/blob/master/examples/refresh_before_expiry.rs)
/// for the full threshold-refresh recipe.
///
/// This is a standalone trait rather than a new method on [`CloneCached`]: adding a required
/// method there would break every external store that implements it. It is also not a supertrait
/// of [`CloneCached`], which would force implementors to provide `CloneCached` for no benefit.
///
/// The deadline is an [`Instant`](crate::time::Instant), not a remaining `Duration`, because a
/// `Duration` cannot distinguish "never expires" from "already expired". It is the same type
/// [`Expires::expires_at`] returns.
///
/// # Return values
///
/// - `(None, None)`: the key is absent. Under the macros this is also what a peek with a key that
///   differs from what the `convert` block builds returns, which is indistinguishable from a cold
///   miss: the code compiles and every call recomputes synchronously.
/// - `(Some(v), None)`: present, with no known deadline. The causes are a TTL disabled at insert
///   time, an [`Expires`] value type that does not override `expires_at`, and a `now + ttl` that
///   overflows [`Instant`](crate::time::Instant) (a TTL on the order of hundreds of years), which
///   the single-owner TTL stores record as "never expires". That last cause is not universal: the
///   sharded TTL stores clamp the TTL to `u64::MAX` nanoseconds instead and report a real
///   far-future deadline.
/// - `(Some(v), Some(t))` with `t` in the future: present, and on the TTL stores ([`TtlCache`],
///   [`LruTtlCache`], [`TtlSortedCache`]) also live. On the [`Expires`]-based stores a future `t`
///   is not evidence of liveness; see the caveat below.
/// - `(Some(v), Some(t))` with `t` in the past: present but expired. The entry is returned and
///   **not** removed, consistent with `cache_peek_with_expiry_status` returning `(Some(v), true)`.
///
/// [`cache_expires_at`](Self::cache_expires_at) reports the same four cases, with a `bool`
/// presence flag in place of the value: `(false, None)` absent, `(true, None)` present with no
/// deadline, `(true, Some(t))` with `t` in the future, and `(true, Some(t))` with `t` in the past.
/// The flag is `true` exactly when `cache_peek_expires_at` returns `Some(v)`, and the deadline is
/// the same in both.
///
/// The presence flag is not redundant: absent and present-but-never-expires call for **opposite**
/// actions in a threshold-refresh policy (fetch the key cold versus do nothing), and a bare
/// `Option<Instant>` return would collapse them into one `None`. That is the same conflation the
/// [`Instant`](crate::time::Instant) return avoids by not being a remaining `Duration`.
///
/// The convention is `now >= t` means expired: a deadline exactly equal to the current instant
/// counts as already past, matching the liveness check the store itself applies.
///
/// # Side effects
///
/// None on either method, identical to the peek they parallel: no LRU promotion, no hits/misses
/// counter increment, no TTL renewal (`refresh_on_hit` does not fire), and no lazy removal of an
/// expired entry.
///
/// # Cost
///
/// [`cache_peek_expires_at`](Self::cache_peek_expires_at) hands back the value, so it carries a
/// `V: Clone` bound and clones the stored value on every call; that clone lands on the read path.
/// [`cache_expires_at`](Self::cache_expires_at) does not: it reads the deadline out of the same
/// non-promoting lookup and never touches the value, so it has no `V: Clone` bound and works on a
/// value type that is not [`Clone`] at all. It is the one to use for a threshold check that only
/// needs the deadline.
///
/// # Reading through a macro-generated static
///
/// A `#[cached]` macro user reaches the store through the generated static's lock. Bind the peek
/// result and drop the guard before calling the cached function or `{fn}_prime_cache`: both take
/// the cache write lock, and `parking_lot`'s `RwLock` is not reentrant, so calling either while
/// the read guard is still alive deadlocks the thread permanently (no panic, no output).
///
/// Two macro forms cannot be used for this at all: `#[cached(in_impl = true)]` emits the cache
/// static inside the method body and generates no `{fn}_prime_cache`, and `#[once]` keeps its own
/// timer around a single `Option` rather than a store implementing this trait.
///
/// # Caveat on the `Expires`-based stores
///
/// [`TtlCache`], [`LruTtlCache`], and [`TtlSortedCache`] store a real deadline and deliver the
/// full contract above. On the [`Expires`]-based stores ([`ExpiringCache`], [`ExpiringLruCache`])
/// the deadline is whatever [`Expires::expires_at`] reports, and there it is advisory only:
/// the default implementation returns `None`, so it is `None` for any value type that does not
/// override it (including entries that are expired). Nothing constrains an [`Expires`] impl to
/// keep `expires_at` and `is_expired` agreeing, and the two can disagree in both directions: `t`
/// can be in the past for an entry `Expires::is_expired` reports live, and `t` can be in the
/// future for an entry `is_expired` reports expired (a token with a fixed deadline that is also
/// revocable). A future `t` is therefore not evidence that the entry is live on these stores.
/// `is_expired` is the authority there, so
/// [`cache_peek_with_expiry_status`](CloneCached::cache_peek_with_expiry_status) remains the
/// authoritative liveness read.
pub trait CacheExpiry<K, V> {
    /// Peek at the value and the instant it expires at, with no read side effects.
    ///
    /// See the [trait docs](CacheExpiry) for the four return shapes, the `now >= t` expiry
    /// convention, the value-clone cost, and the `Expires`-store caveat.
    fn cache_peek_expires_at<Q>(&self, key: &Q) -> (Option<V>, Option<crate::time::Instant>)
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
        V: Clone;

    /// Ergonomic alias for [`cache_peek_expires_at`](Self::cache_peek_expires_at).
    fn peek_expires_at<Q>(&self, key: &Q) -> (Option<V>, Option<crate::time::Instant>)
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
        V: Clone,
    {
        self.cache_peek_expires_at(key)
    }

    /// Read whether `key` is present and the instant it expires at, without the value.
    ///
    /// Returns `(present, deadline)`: `(false, None)` absent, `(true, None)` present with no
    /// deadline, `(true, Some(t))` with `t` in the future, `(true, Some(t))` with `t` in the past
    /// (present but expired, and **not** removed). The presence flag separates the absent key
    /// from the never-expiring entry, which call for opposite actions.
    ///
    /// Same lookup and the same no-side-effect contract as
    /// [`cache_peek_expires_at`](Self::cache_peek_expires_at), but the value is never touched, so
    /// there is no `V: Clone` bound and no clone on the read path. See the
    /// [trait docs](CacheExpiry) for the `now >= t` expiry convention and the `Expires`-store
    /// caveat.
    ///
    /// This is a required method.
    fn cache_expires_at<Q>(&self, key: &Q) -> (bool, Option<crate::time::Instant>)
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized;

    /// Ergonomic alias for [`cache_expires_at`](Self::cache_expires_at).
    ///
    /// This is the cache-side read: it takes a key and answers for one entry. It is distinct from
    /// [`Expires::expires_at`], which a *value* type implements and which takes no key, and from
    /// [`CacheValue::expires_at`], which reports the deadline of a value already in hand.
    fn expires_at<Q>(&self, key: &Q) -> (bool, Option<crate::time::Instant>)
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        self.cache_expires_at(key)
    }
}

/// Concurrent analogue of [`CloneCached`] for the internally-synchronized sharded stores.
///
/// Like [`CloneCached`], [`cache_get_with_expiry_status`](ConcurrentCloneCached::cache_get_with_expiry_status)
/// returns a present-but-expired entry **without removing it**, so callers (e.g.
/// [`result_fallback`](macro@crate::macros::concurrent_cached)) can fall back to the stale
/// value if a refresh fails. Takes `&self` instead of `&mut self` because sharded stores are
/// internally synchronized and never need exclusive ownership from the caller.
///
/// Implemented by the four expiry-capable sharded stores:
/// [`ShardedTtlCache`], [`ShardedLruTtlCache`], [`ShardedExpiringCache`], and
/// [`ShardedExpiringLruCache`].
/// Non-expiry stores ([`ShardedUnboundCache`], [`ShardedLruCache`]) do not implement this trait,
/// mirroring how [`CloneCached`] is absent on [`UnboundCache`] and [`LruCache`].
///
/// **Why `&K` instead of `&Q` (`Borrow<Q>`)**: same reason as [`ConcurrentCached`] — the
/// concurrent cache trait family includes external stores that must serialize the key, so
/// the trait takes `&K` directly rather than a generic `Borrow<Q>` that carries no
/// serialization guarantee.
///
/// **Race window**: between the expiry check and the subsequent re-cache on `Err`, another
/// thread may have stored a fresh `Ok`. The re-cache will overwrite that fresh value, but
/// the outcome is only a redundant function call on the next expiry — not data corruption.
///
/// # Examples
///
/// ```rust
/// # #[cfg(feature = "time_stores")]
/// # {
/// use cached::{ConcurrentCloneCached, ShardedTtlCache};
/// use cached::time::Duration;
///
/// let c = ShardedTtlCache::builder().ttl(Duration::from_secs(60)).build().unwrap();
/// c.set("k".to_string(), 1_i32);
/// assert_eq!(c.cache_get_with_expiry_status(&"k".to_string()), (Some(1_i32), false)); // live
/// assert_eq!(c.cache_get_with_expiry_status(&"x".to_string()), (None, false));        // absent
/// // a present-but-expired entry yields (Some(v), true)
/// # }
/// ```
pub trait ConcurrentCloneCached<K, V> {
    /// Look up a cached value and report whether the found entry is expired.
    ///
    /// Returns `(value, expired)` where:
    /// - `(None, false)` — key not present
    /// - `(Some(v), false)` — key present and live (hits counter incremented)
    /// - `(Some(v), true)` — key present but expired; the stale value is returned so callers
    ///   can fall back to it if a refresh fails. The entry is **not** removed from the cache
    ///   and eviction counters are **not** incremented (misses counter incremented).
    ///
    /// Unlike [`ConcurrentCached::cache_get`], this never removes an expired entry — it
    /// intentionally leaves it in place so it can be returned as a stale fallback. Expired
    /// entries are swept by a subsequent `cache_get`, an explicit `cache_remove`, or `evict()`.
    fn cache_get_with_expiry_status(&self, key: &K) -> (Option<V>, bool);

    /// Ergonomic alias for [`cache_get_with_expiry_status`](Self::cache_get_with_expiry_status).
    fn get_with_expiry_status(&self, key: &K) -> (Option<V>, bool) {
        self.cache_get_with_expiry_status(key)
    }

    /// Look up a cached value and report whether the found entry is expired without any read
    /// side effects.
    ///
    /// This is a required method. Implementations must satisfy the contract: same
    /// `(value, expired)` return shape as
    /// [`cache_get_with_expiry_status`](Self::cache_get_with_expiry_status), but the read
    /// must not increment hits or misses counters, must not update LRU recency, and must
    /// not renew the TTL.
    ///
    /// Returns `(Some(v), true)` for a present-but-expired entry, `(None, false)` for an
    /// absent key, `(Some(v), false)` for a live entry.
    ///
    /// This is used on the `force_refresh` bypass path of `#[concurrent_cached(result_fallback = true)]`
    /// to capture the stale fallback value without touching counters or recency.
    fn cache_peek_with_expiry_status(&self, key: &K) -> (Option<V>, bool);

    /// Ergonomic alias for [`cache_peek_with_expiry_status`](Self::cache_peek_with_expiry_status).
    fn peek_with_expiry_status(&self, key: &K) -> (Option<V>, bool) {
        self.cache_peek_with_expiry_status(key)
    }
}

/// Concurrent analogue of [`CacheExpiry`] for the internally-synchronized sharded stores: a
/// side-effect-free per-key expiry read.
///
/// [`cache_peek_expires_at`](Self::cache_peek_expires_at) returns the stored value together with
/// the instant it expires at; [`cache_expires_at`](Self::cache_expires_at) returns only whether
/// the key is present plus that instant, with no value, no clone, and no `V: Clone` bound. Both
/// are reads, and both let a caller express a "remaining TTL below a threshold" predicate that
/// [`cache_peek_with_expiry_status`](ConcurrentCloneCached::cache_peek_with_expiry_status),
/// which reports only a boolean, cannot:
///
/// ```rust
/// # #[cfg(feature = "time_stores")]
/// # {
/// use cached::{ConcurrentCacheExpiry, ShardedTtlCache};
/// use cached::time::{Duration, Instant};
///
/// let c = ShardedTtlCache::builder().ttl(Duration::from_secs(60)).build().unwrap();
/// c.set("k".to_string(), 1_i32);
///
/// // This trait takes `&K`, not a `Borrow<Q>` form, so the key is a `&String`.
/// let (value, expires_at) = c.peek_expires_at(&"k".to_string());
/// assert_eq!(value, Some(1_i32));
///
/// let deadline = expires_at.expect("a configured ttl records a deadline");
/// let remaining = deadline.saturating_duration_since(Instant::now());
/// assert!(remaining > Duration::from_secs(50));
/// // the policy: refresh once less than 10s of the 60s ttl is left
/// let needs_refresh = remaining < Duration::from_secs(10);
/// assert!(!needs_refresh);
///
/// assert_eq!(c.peek_expires_at(&"absent".to_string()), (None, None));
///
/// // The same policy without the value: `(present, deadline)`, no clone under the shard lock.
/// let (present, expires_at) = c.expires_at(&"k".to_string());
/// assert!(present);
/// let deadline = expires_at.expect("a configured ttl records a deadline");
/// assert!(deadline.saturating_duration_since(Instant::now()) > Duration::from_secs(50));
/// assert!(deadline > Instant::now());
///
/// // Absent and never-expires are distinct, and call for opposite actions.
/// assert_eq!(c.expires_at(&"absent".to_string()), (false, None)); // fetch it
/// # }
/// ```
///
/// See [`examples/refresh_before_expiry.rs`](https://github.com/jaemk/cached/blob/master/examples/refresh_before_expiry.rs)
/// for the full threshold-refresh recipe.
///
/// Unlike the `set` / `get` / `len` / `contains` / `peek` the sharded stores expose inherently,
/// these methods (and their `peek_expires_at` / `expires_at` aliases) are reachable only through
/// this trait, so `use cached::ConcurrentCacheExpiry;` is required at the call site.
///
/// Like [`CacheExpiry`], this is a standalone trait rather than a new required method on
/// [`ConcurrentCloneCached`] (which would break external store implementations) and not a
/// supertrait of it. The return is a bare tuple, mirroring [`ConcurrentCloneCached`] rather than
/// the `Result`-returning [`ConcurrentCached`] family.
///
/// **Why `&K` instead of `&Q` (`Borrow<Q>`)**: same reason as [`ConcurrentCloneCached`], the
/// concurrent cache trait family includes external stores that must serialize the key, so the
/// trait takes `&K` directly rather than a generic `Borrow<Q>` that carries no serialization
/// guarantee.
///
/// # Return values
///
/// - `(None, None)`: the key is absent. Under the macros this is also what a peek with a key that
///   differs from what the `convert` block builds returns, which is indistinguishable from a cold
///   miss: the code compiles and every call recomputes synchronously.
/// - `(Some(v), None)`: present, with no known deadline (TTL disabled at insert time, or an
///   [`Expires`] value type that does not override `expires_at`). The sharded TTL stores clamp
///   their TTL to `u64::MAX` nanoseconds, so an oversized TTL yields a real far-future deadline
///   here rather than the `None` the single-owner TTL stores report on `Instant` overflow.
/// - `(Some(v), Some(t))` with `t` in the future: present, and on the TTL stores
///   ([`ShardedTtlCache`], [`ShardedLruTtlCache`]) also live. On the [`Expires`]-based stores a
///   future `t` is not evidence of liveness; see the caveat below.
/// - `(Some(v), Some(t))` with `t` in the past: present but expired. The entry is returned and
///   **not** removed, consistent with `cache_peek_with_expiry_status` returning `(Some(v), true)`.
///
/// [`cache_expires_at`](Self::cache_expires_at) reports the same four cases, with a `bool`
/// presence flag in place of the value: `(false, None)` absent, `(true, None)` present with no
/// deadline, `(true, Some(t))` with `t` in the future, and `(true, Some(t))` with `t` in the past.
/// The flag is `true` exactly when `cache_peek_expires_at` returns `Some(v)`, and the deadline is
/// the same in both.
///
/// The presence flag is not redundant: absent and present-but-never-expires call for **opposite**
/// actions in a threshold-refresh policy (fetch the key cold versus do nothing), and a bare
/// `Option<Instant>` return would collapse them into one `None`. That is the same conflation the
/// [`Instant`](crate::time::Instant) return avoids by not being a remaining `Duration`.
///
/// The convention is `now >= t` means expired: a deadline exactly equal to the current instant
/// counts as already past, matching the liveness check the store itself applies.
///
/// # Side effects
///
/// None on either method, identical to the peek they parallel: no LRU promotion, no hits/misses
/// counter increment, no TTL renewal (`refresh_on_hit` does not fire), and no lazy removal of an
/// expired entry. Both take only a shard read lock.
///
/// # Cost
///
/// [`cache_peek_expires_at`](Self::cache_peek_expires_at) hands back the value, so it carries a
/// `V: Clone` bound and clones the stored value under the shard lock on every call.
/// [`cache_expires_at`](Self::cache_expires_at) does not: it reads the deadline out of the same
/// lookup and never touches the value, so it has no `V: Clone` bound and works on a value type
/// that is not [`Clone`] at all. It is the one to use for a threshold check that only needs the
/// deadline.
///
/// # Reading through a macro-generated static
///
/// A `#[concurrent_cached]` static holds the sharded store itself (`LazyLock<Store>` for sync
/// functions, `OnceCell<Store>` for async ones), so a peek there takes no outer guard. The
/// single-owner side differs: a `#[cached]` macro user reaches the store through the
/// generated static's lock. Bind the peek result and drop the guard before calling the cached
/// function or `{fn}_prime_cache`: both take the cache write lock, and `parking_lot`'s `RwLock` is
/// not reentrant, so calling either while the read guard is still alive deadlocks the thread
/// permanently (no panic, no output).
///
/// Two macro forms cannot be used for this at all: `#[concurrent_cached(in_impl = true)]` (like
/// `#[cached(in_impl = true)]`) emits the cache static inside the method body and generates no
/// `{fn}_prime_cache`, and `#[once]` keeps its own timer around a single `Option` rather than a
/// store implementing this trait.
///
/// # Caveat on the `Expires`-based stores
///
/// [`ShardedTtlCache`] and [`ShardedLruTtlCache`] store a real deadline and deliver the full
/// contract above. On [`ShardedExpiringCache`] and [`ShardedExpiringLruCache`] the deadline is
/// whatever [`Expires::expires_at`] reports, and there it is advisory only: the default
/// implementation returns `None`, so it is `None` for any value type that does not override it
/// (including entries that are expired). Nothing constrains an [`Expires`] impl to keep
/// `expires_at` and `is_expired` agreeing, and the two can disagree in both directions: `t` can be
/// in the past for an entry `Expires::is_expired` reports live, and `t` can be in the future for
/// an entry `is_expired` reports expired (a token with a fixed deadline that is also revocable).
/// A future `t` is therefore not evidence that the entry is live on these stores. `is_expired` is
/// the authority there, so
/// [`cache_peek_with_expiry_status`](ConcurrentCloneCached::cache_peek_with_expiry_status)
/// remains the authoritative liveness read.
///
/// **Both reads take `&K`, not a borrowed form.** The sharded stores' inherent
/// `get`/`peek`/`contains` accept any `&Q` where `K: Borrow<Q>` (bounded on
/// [`ShardHasher<Q>`](ShardHasher)), and the single-owner [`CacheExpiry`] mirror is generic over
/// `Q` as well, but these methods stay on the `&K` concurrent trait surface for the reason given
/// on [`ConcurrentCached`]. So on a `ShardedTtlCache<String, V>`, `cache.peek("a")` compiles while
/// `cache.expires_at("a")` does not: pass `&key` (allocating a `String` if all you hold is a
/// `&str`).
pub trait ConcurrentCacheExpiry<K, V> {
    /// Peek at the value and the instant it expires at, with no read side effects.
    ///
    /// Requires `use cached::ConcurrentCacheExpiry;` at the call site: the sharded stores expose
    /// no inherent shim for it. See the [trait docs](ConcurrentCacheExpiry) for the four return
    /// shapes, the `now >= t` expiry convention, the value-clone cost, and the `Expires`-store
    /// caveat.
    fn cache_peek_expires_at(&self, key: &K) -> (Option<V>, Option<crate::time::Instant>)
    where
        V: Clone;

    /// Ergonomic alias for [`cache_peek_expires_at`](Self::cache_peek_expires_at).
    fn peek_expires_at(&self, key: &K) -> (Option<V>, Option<crate::time::Instant>)
    where
        V: Clone,
    {
        self.cache_peek_expires_at(key)
    }

    /// Read whether `key` is present and the instant it expires at, without the value.
    ///
    /// Returns `(present, deadline)`: `(false, None)` absent, `(true, None)` present with no
    /// deadline, `(true, Some(t))` with `t` in the future, `(true, Some(t))` with `t` in the past
    /// (present but expired, and **not** removed). The presence flag separates the absent key
    /// from the never-expiring entry, which call for opposite actions.
    ///
    /// Same lookup and the same no-side-effect contract as
    /// [`cache_peek_expires_at`](Self::cache_peek_expires_at), but the value is never touched, so
    /// there is no `V: Clone` bound and no clone under the shard lock. Requires
    /// `use cached::ConcurrentCacheExpiry;` at the call site. See the
    /// [trait docs](ConcurrentCacheExpiry) for the `now >= t` expiry convention and the
    /// `Expires`-store caveat.
    ///
    /// This is a required method.
    fn cache_expires_at(&self, key: &K) -> (bool, Option<crate::time::Instant>);

    /// Ergonomic alias for [`cache_expires_at`](Self::cache_expires_at).
    ///
    /// This is the cache-side read: it takes a key and answers for one entry. It is distinct from
    /// [`Expires::expires_at`], which a *value* type implements and which takes no key, and from
    /// [`CacheValue::expires_at`], which reports the deadline of a value already in hand.
    fn expires_at(&self, key: &K) -> (bool, Option<crate::time::Instant>) {
        self.cache_expires_at(key)
    }
}

/// TTL management for single-owner time-bounded cache stores.
///
/// Implemented by the `&mut self` single-owner in-memory stores [`TtlCache`],
/// [`LruTtlCache`], and [`TtlSortedCache`].
///
/// Internally-synchronized concurrent stores (the sharded TTL stores, `RedisCache`,
/// `AsyncRedisCache`, and `RedbCache`) do **not** implement this trait. They are held
/// behind an `Arc`/`static` and cannot offer `&mut self`. Manage their TTL through the
/// `&self` methods on the [`ConcurrentCacheTtl`] trait
/// (`ttl`/`set_ttl`/`try_set_ttl`/`unset_ttl`) instead.
///
/// Refresh-on-hit is **not** part of this trait. It lives on [`CacheRefreshOnHit`], which
/// [`TtlSortedCache`] deliberately does not implement: its entries are ordered by expiry, so
/// extending one entry's deadline on read would break the ordering the store is built around.
/// Bound on [`CacheRefreshOnHit`] when generic code needs the knob, and on `CacheTtl` when it
/// only needs the TTL itself.
///
/// The trait itself is always available (mirroring [`ConcurrentCacheTtl`]), so external stores
/// can implement it without enabling `time_stores`; the built-in implementations on
/// [`TtlCache`], [`LruTtlCache`], and [`TtlSortedCache`] require the `time_stores` feature.
pub trait CacheTtl {
    /// Return the TTL applied to newly inserted entries.
    #[must_use]
    fn ttl(&self) -> Option<Duration>;

    /// Set the TTL for newly inserted entries, returning the previous value (or `None`
    /// if expiry was disabled).
    ///
    /// A zero `ttl` disables expiry: subsequently inserted entries never expire. This is
    /// equivalent to [`unset_ttl`](Self::unset_ttl). Pre-existing entries keep their
    /// original expiry. Use [`try_set_ttl`](Self::try_set_ttl) if you want a zero `ttl`
    /// rejected instead.
    fn set_ttl(&mut self, ttl: Duration) -> Option<Duration>;

    /// Validated variant of [`set_ttl`](Self::set_ttl): returns [`SetTtlError::ZeroTtl`]
    /// when `ttl` is zero instead of storing it. This is the strict "give me a real ttl"
    /// path; to disable expiry, call [`set_ttl`](Self::set_ttl) with a zero `Duration` or
    /// [`unset_ttl`](Self::unset_ttl).
    fn try_set_ttl(&mut self, ttl: Duration) -> Result<Option<Duration>, crate::SetTtlError> {
        if ttl.is_zero() {
            return Err(crate::SetTtlError::ZeroTtl);
        }
        Ok(self.set_ttl(ttl))
    }

    /// Remove the TTL so entries are retained indefinitely.
    ///
    /// No-op for stores that cannot retain values indefinitely.
    fn unset_ttl(&mut self) -> Option<Duration>;
}

/// Refresh-on-hit control for single-owner time-bounded cache stores.
///
/// Split out of [`CacheTtl`] so that satisfying the bound implies the capability. Implemented by
/// the `&mut self` single-owner stores that can genuinely extend an entry's deadline on read:
/// [`TtlCache`] and [`LruTtlCache`].
///
/// [`TtlSortedCache`] implements [`CacheTtl`] but **not** this trait. Its entries are kept in a
/// deadline-ordered index, so pushing one entry's expiry forward on a read would invalidate that
/// ordering; the store therefore has no refresh-on-hit mode at all. Previously it satisfied the
/// combined trait with a `set_refresh_on_hit` that ignored its argument and always returned
/// `false`, which is indistinguishable from "the flag was already off" to a generic caller. With
/// the capability in its own trait the difference is a compile error instead of a silent no-op.
///
/// The trait itself is always available (mirroring [`CacheTtl`] and
/// [`ConcurrentCacheRefreshOnHit`]), so external stores can implement it without enabling
/// `time_stores`; the built-in implementations require the `time_stores` feature.
///
/// ```rust
/// # #[cfg(feature = "time_stores")] {
/// use cached::{CacheRefreshOnHit, TtlCache};
/// use std::time::Duration;
///
/// let mut cache: TtlCache<String, u32> = TtlCache::new(Duration::from_secs(60));
/// assert!(!cache.refresh_on_hit());
/// // The setter reports the previous value.
/// assert!(!cache.set_refresh_on_hit(true));
/// assert!(cache.refresh_on_hit());
/// # }
/// ```
pub trait CacheRefreshOnHit {
    /// Return `true` if cache hits refresh the TTL of the accessed entry.
    #[must_use]
    fn refresh_on_hit(&self) -> bool;

    /// Set whether cache hits should refresh the TTL. Returns the previous value.
    ///
    /// Every implementor honors this: the returned flag is the state the store was actually in
    /// before the call, and the new state takes effect for subsequent hits.
    fn set_refresh_on_hit(&mut self, refresh: bool) -> bool;
}

/// Capacity resize for single-owner stores that carry a size bound.
///
/// Implemented by the four bounded single-owner stores: [`LruCache`], [`LruTtlCache`],
/// [`ExpiringLruCache`], and [`TtlSortedCache`]. Each of them already exposes
/// `set_max_size`/`try_set_max_size` inherently; this trait is what makes the pair reachable from
/// generic code holding a `T: CacheSetMaxSize` rather than a concrete store type.
///
/// [`UnboundCache`], [`TtlCache`], and [`ExpiringCache`] deliberately do **not** implement it.
/// None of them has a bound to change: the unbounded store has none by definition, and the two
/// time-only stores are sized by what has not yet expired (their builders take an
/// `initial_capacity` allocation hint consumed once at build time, which is not a live bound). A
/// stub returning `None` would be indistinguishable to a generic caller from a store that really
/// resized, so the capability lives in its own trait and those stores are simply left off it,
/// exactly as [`TtlSortedCache`] is left off [`CacheRefreshOnHit`]. The Redis and redb stores have
/// no client-side capacity at all; capacity there is a server or database property.
///
/// Reading the bound needs no trait beyond [`Cached`]: `cache_capacity` is already a method on
/// [`Cached`] (defaulted to `None`, overridden by every bounded store), so only the setters were
/// inherent-only.
///
/// Inherent methods take call-site priority over trait methods, so this changes nothing for code
/// that names a concrete store. It only adds the generic route:
///
/// ```rust
/// use cached::{CacheSetMaxSize, CachedExt, LruCache};
///
/// fn shrink<T: CacheSetMaxSize>(cache: &mut T, max_size: usize) -> Option<usize> {
///     cache.set_max_size(max_size)
/// }
///
/// let mut cache: LruCache<u32, u32> = LruCache::new(10);
/// cache.set(1, 1);
/// cache.set(2, 2);
/// assert_eq!(shrink(&mut cache, 1), Some(10)); // the previous bound
/// assert_eq!(cache.len(), 1);                  // the shrink evicted eagerly
/// ```
///
/// The `&self` mirror for the internally-synchronized stores is
/// [`ConcurrentCacheSetMaxSize`]. The trait itself is not feature-gated (mirroring [`CacheTtl`]),
/// so external stores can implement it in any feature configuration; the built-in impls on
/// [`LruTtlCache`] and [`TtlSortedCache`] require `time_stores`.
pub trait CacheSetMaxSize {
    /// Change the maximum number of entries, returning the previous bound.
    ///
    /// Shrinking below the current entry count evicts eagerly, before this returns: entries are
    /// removed until the cache fits the new bound, firing `on_evict` and counting an eviction for
    /// each removed entry. Growing does not pre-allocate; the backing stores grow on demand.
    ///
    /// **Which entries are dropped inverts across implementors**, so generic code holding only a
    /// `T: CacheSetMaxSize` bound cannot assume either policy: [`LruCache`], [`LruTtlCache`], and
    /// [`ExpiringLruCache`] drop the least-recently-used entries, while [`TtlSortedCache`] drops
    /// the entries closest to expiring (it shrinks through `retain_latest`, keeping the
    /// longest-lived entries).
    ///
    /// The return is the previous bound. It is `Some` on every store that was already bounded,
    /// which is all of them except [`TtlSortedCache`], the one built-in store that can be
    /// constructed with no bound and so can report `None`.
    ///
    /// # Panics
    ///
    /// Panics if `max_size` is 0. Use [`try_set_max_size`](Self::try_set_max_size) to validate
    /// first and avoid the panic.
    fn set_max_size(&mut self, max_size: usize) -> Option<usize>;

    /// Validated variant of [`set_max_size`](Self::set_max_size): rejects an invalid `max_size`
    /// instead of panicking. On success it is [`set_max_size`](Self::set_max_size), same return
    /// and same eager eviction on shrink.
    ///
    /// # Errors
    ///
    /// Returns [`SetMaxSizeError::ZeroMaxSize`] if `max_size` is 0. The
    /// [`CapacityOverflow`](SetMaxSizeError::CapacityOverflow) variant is never returned by a
    /// single-owner store; it belongs to the sharded implementors of
    /// [`ConcurrentCacheSetMaxSize`], which divide the bound across shards.
    fn try_set_max_size(
        &mut self,
        max_size: usize,
    ) -> Result<Option<usize>, crate::SetMaxSizeError>;
}

/// Clearing a single-owner store with `on_evict` notification.
///
/// [`Cached::cache_clear`] removes every entry silently: it does not fire `on_evict` and does not
/// count evictions. `cache_clear_with_on_evict` is the notifying form, for stores whose `on_evict`
/// callback releases something tracked outside the cache.
///
/// Implemented by all seven single-owner in-memory stores: [`UnboundCache`], [`LruCache`],
/// [`TtlCache`], [`LruTtlCache`], [`TtlSortedCache`], [`ExpiringCache`], and
/// [`ExpiringLruCache`]. Unlike [`CacheSetMaxSize`], there is no partial coverage to reason about
/// here: every store with an `on_evict` callback has the method. The trait exists to make it
/// reachable through a bound, and the split from [`ConcurrentCacheClearWithOnEvict`] is receiver
/// symmetry (`&mut self` versus `&self`), not a difference in capability. The Redis and redb
/// stores implement neither, having no `on_evict` mechanism to notify.
///
/// ```rust
/// use cached::{CacheClearWithOnEvict, CachedExt, UnboundCache};
/// use std::sync::Arc;
/// use std::sync::atomic::{AtomicUsize, Ordering};
///
/// fn drain<T: CacheClearWithOnEvict>(cache: &mut T) {
///     cache.cache_clear_with_on_evict();
/// }
///
/// let evicted = Arc::new(AtomicUsize::new(0));
/// let counter = evicted.clone();
/// let mut cache = UnboundCache::builder()
///     .on_evict(move |_k: &u32, _v: &u32| {
///         counter.fetch_add(1, Ordering::Relaxed);
///     })
///     .build()
///     .unwrap();
/// cache.set(1, 10);
/// cache.set(2, 20);
///
/// drain(&mut cache);
/// assert_eq!(cache.len(), 0);
/// assert_eq!(evicted.load(Ordering::Relaxed), 2);
/// ```
///
/// The trait is not feature-gated; only the built-in impls on the `time_stores` stores are.
pub trait CacheClearWithOnEvict {
    /// Remove every entry, firing `on_evict` for each removed entry and counting it as an
    /// eviction.
    ///
    /// The eviction count does not depend on whether an `on_evict` callback is configured, on the
    /// stores that track evictions at all ([`UnboundCache`] has no evictions counter). Contrast
    /// [`Cached::cache_clear`], which removes the same entries with neither side effect.
    fn cache_clear_with_on_evict(&mut self);
}

/// Memoize an async closure over a synchronous in-memory [`Cached`] store.
///
/// This is not a general async cache surface: in-memory [`Cached`] stores never touch IO, so
/// they are fully synchronous. `CachedGetOrSetAsync` only adds the get-or-set pattern for an
/// async closure - on a miss it awaits the caller's closure and inserts the result - and
/// sidesteps the borrow-check friction a hand-rolled version hits. For a concurrent store with
/// its own synchronization (the in-memory sharded stores, or the IO-backed redis and redb
/// stores), use [`ConcurrentCachedAsync`], which mirrors the full sync surface; do not reach for
/// this trait there.
///
/// The methods are `async_`-prefixed so importing this alongside [`CachedExt`] (common, since the
/// in-memory stores implement both) does not make `get_or_set_with` ambiguous at the call site.
#[cfg(feature = "async_core")]
#[cfg_attr(docsrs, doc(cfg(feature = "async_core")))]
pub trait CachedGetOrSetAsync<K, V> {
    /// Get the value for `k`, or compute and insert it by awaiting `f` on a miss.
    ///
    /// The async counterpart of [`CachedExt::get_or_set_with`]. It is `async_`-prefixed
    /// so that importing both [`CachedExt`] and [`CachedGetOrSetAsync`] (common, since the
    /// in-memory stores implement both) does not make `get_or_set_with` ambiguous
    /// at the call site.
    ///
    /// Returns `&V`. Use
    /// [`async_cache_get_or_set_with_mut`](CachedGetOrSetAsync::async_cache_get_or_set_with_mut) for a
    /// mutable reference.
    ///
    /// This default returns a `Send` future, so it carries `Self: Send, K: Send`
    /// (the future captures `&mut self` and `k` across the await). It is only a
    /// convenience wrapper over
    /// [`async_cache_get_or_set_with_mut`](CachedGetOrSetAsync::async_cache_get_or_set_with_mut).
    fn async_cache_get_or_set_with<'a, F, Fut>(
        &'a mut self,
        k: K,
        f: F,
    ) -> impl Future<Output = &'a V> + Send + 'a
    where
        Self: Send,
        K: Send + 'a,
        V: Send + 'a,
        F: FnOnce() -> Fut + Send + 'a,
        Fut: Future<Output = V> + Send + 'a,
    {
        async move { &*self.async_cache_get_or_set_with_mut(k, f).await }
    }

    /// The mutable counterpart of
    /// [`async_cache_get_or_set_with`](CachedGetOrSetAsync::async_cache_get_or_set_with): returns
    /// `&mut V`. Stores implement this method; the shared-reference variant
    /// delegates to it.
    fn async_cache_get_or_set_with_mut<'a, F, Fut>(
        &'a mut self,
        k: K,
        f: F,
    ) -> impl Future<Output = &'a mut V> + Send + 'a
    where
        K: 'a,
        V: Send + 'a,
        F: FnOnce() -> Fut + Send + 'a,
        Fut: Future<Output = V> + Send + 'a;

    /// Like [`async_cache_get_or_set_with`](CachedGetOrSetAsync::async_cache_get_or_set_with), but
    /// `f` is fallible: on a miss the value is cached only if `f` resolves to
    /// `Ok`, and an `Err` is returned without caching.
    ///
    /// Returns `Result<&V, E>`. Use
    /// [`async_cache_try_get_or_set_with_mut`](CachedGetOrSetAsync::async_cache_try_get_or_set_with_mut)
    /// for a mutable reference.
    ///
    /// Like [`async_cache_get_or_set_with`](CachedGetOrSetAsync::async_cache_get_or_set_with), this
    /// default returns a `Send` future and so carries `Self: Send, K: Send`.
    fn async_cache_try_get_or_set_with<'a, F, Fut, E>(
        &'a mut self,
        k: K,
        f: F,
    ) -> impl Future<Output = Result<&'a V, E>> + Send + 'a
    where
        Self: Send,
        K: Send + 'a,
        V: Send + 'a,
        E: 'a,
        F: FnOnce() -> Fut + Send + 'a,
        Fut: Future<Output = Result<V, E>> + Send + 'a,
    {
        async move {
            self.async_cache_try_get_or_set_with_mut(k, f)
                .await
                .map(|v| &*v)
        }
    }

    /// The mutable counterpart of
    /// [`async_cache_try_get_or_set_with`](CachedGetOrSetAsync::async_cache_try_get_or_set_with):
    /// returns `Result<&mut V, E>`.
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
        Fut: Future<Output = Result<V, E>> + Send + 'a;
}

/// Shared base of the concurrent cache traits.
///
/// [`ConcurrentCached`] and [`ConcurrentCachedAsync`] both extend this trait, which owns the
/// associated [`Error`](Self::Error) type and the cheap introspection methods
/// ([`cache_size`](Self::cache_size), [`cache_is_empty`](Self::cache_is_empty),
/// [`cache_hits`](Self::cache_hits), [`cache_misses`](Self::cache_misses),
/// [`cache_capacity`](Self::cache_capacity), [`cache_evictions`](Self::cache_evictions),
/// and [`metrics`](Self::metrics)). Hoisting these into a single base means a store that
/// implements both concurrent traits declares them once and can call them through a shared
/// reference without ambiguity.
///
/// The metric accessors mirror those on [`Cached`]/[`CachedExt`] for the non-concurrent family.
/// The sharded in-memory stores override them to aggregate across all shards; external stores
/// (Redis, Redb) leave the defaults, which return `None` / an empty `CacheMetrics`.
///
/// Implementors do not normally name this trait directly: implementing [`ConcurrentCached`] or
/// [`ConcurrentCachedAsync`] requires a `ConcurrentCacheBase` impl, which is where the
/// `type Error` and any `cache_size`/metric overrides live.
pub trait ConcurrentCacheBase {
    /// The error type returned by fallible cache operations.
    ///
    /// The bound `std::error::Error + Send + Sync + 'static` lets generic code call
    /// `.unwrap()` on `Result<_, Self::Error>`, propagate with `?` into
    /// `Box<dyn std::error::Error>`, and send/share the error across threads without
    /// additional where-clauses at every use site.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Report the number of entries currently held by the store, if the store can
    /// determine it cheaply.
    ///
    /// Returns `Ok(Some(n))` for stores that track their own size (all in-memory sharded
    /// stores), and `Ok(None)` for stores that cannot answer without an expensive or
    /// semantically-ambiguous query — `RedisCache` / `AsyncRedisCache` (the key count is a
    /// server-side `DBSIZE`/`SCAN` over a shared keyspace, not just this cache's entries) and
    /// `RedbCache` (an `O(n)` scan of the backing table). Those stores return `Ok(None)` rather
    /// than pay that cost implicitly. For `RedisCache` / `AsyncRedisCache` you can query the server
    /// directly if you need a count; for `RedbCache` call `RedbCache::scan_stats()`, an explicit
    /// full scan that also reports expired and undecodable entries and the oldest/newest write
    /// time.
    ///
    /// This is the concurrent analogue of [`Cached::cache_size`], widened to
    /// `Result<Option<usize>, _>` because concurrent stores may be fallible and may not know
    /// their size. The default returns `Ok(None)`.
    ///
    /// # Errors
    ///
    /// Should return `Self::Error` if determining the size fails.
    fn cache_size(&self) -> Result<Option<usize>, Self::Error> {
        Ok(None)
    }

    /// Return `Ok(Some(true))` if the cache is known to be empty, `Ok(Some(false))` if
    /// known non-empty, or `Ok(None)` if the size is unknown.
    ///
    /// This is derived from [`cache_size`](Self::cache_size). Note the sharded stores also
    /// expose an inherent `is_empty(&self) -> bool`, which takes priority at the call site
    /// (`store.is_empty()` returns a bare `bool`); use fully-qualified syntax
    /// `ConcurrentCacheBase::cache_is_empty(&store)` to call this trait method.
    fn cache_is_empty(&self) -> Result<Option<bool>, Self::Error> {
        Ok(self.cache_size()?.map(|n| n == 0))
    }

    /// Return the number of times a cached value was successfully retrieved, if tracked.
    ///
    /// The sharded in-memory stores track per-shard hit counters and sum them here.
    /// External stores (Redis, Redb) do not track hits and return `None`.
    ///
    /// This mirrors [`Cached::cache_hits`] on the non-concurrent family.
    #[must_use]
    fn cache_hits(&self) -> Option<u64> {
        None
    }

    /// Return the number of times a cached value was not found, if tracked.
    ///
    /// The sharded in-memory stores track per-shard miss counters and sum them here.
    /// External stores (Redis, Redb) do not track misses and return `None`.
    ///
    /// This mirrors [`Cached::cache_misses`] on the non-concurrent family.
    #[must_use]
    fn cache_misses(&self) -> Option<u64> {
        None
    }

    /// Return the cache capacity, if bounded.
    ///
    /// "Capacity" here means the eviction bound — the configured `max_size` — not pre-allocated
    /// storage like `Vec::capacity`. Bounded stores (e.g. `ShardedLruCache`, `ShardedLruTtlCache`,
    /// `ShardedExpiringLruCache`) return `Some(total_capacity)`; unbounded stores return `None`.
    ///
    /// This mirrors [`Cached::cache_capacity`] on the non-concurrent family.
    #[must_use]
    fn cache_capacity(&self) -> Option<usize> {
        None
    }

    /// Return the number of entries evicted from the cache, if tracked.
    ///
    /// Stores that track evictions (bounded stores and expiry-capable stores) return
    /// `Some(count)`. Unbounded stores without expiry (e.g. `ShardedUnboundCache`)
    /// return `None`.
    ///
    /// This mirrors [`Cached::cache_evictions`] on the non-concurrent family.
    #[must_use]
    fn cache_evictions(&self) -> Option<u64> {
        None
    }

    /// Return a snapshot of cache metrics.
    ///
    /// Aggregates hits, misses, evictions, entry count, and capacity across all shards (for
    /// sharded stores). The sharded stores also expose an inherent `metrics()` method that
    /// returns the same `CacheMetrics` without needing this trait in scope; this trait
    /// method exists so generic code over a `ConcurrentCacheBase` bound can read metrics
    /// uniformly.
    ///
    /// Note: `shards()` and `shard_sizes()` are inherent-only on the sharded stores - they
    /// are sharding-specific and not part of the generic concurrent base trait.
    ///
    /// This mirrors the default [`CachedExt::metrics`] on the non-concurrent family.
    #[must_use]
    fn metrics(&self) -> CacheMetrics {
        let entry_count = self.cache_size().ok().flatten();
        CacheMetrics {
            hits: self.cache_hits(),
            misses: self.cache_misses(),
            evictions: self.cache_evictions(),
            entry_count,
            capacity: self.cache_capacity(),
        }
    }
}

/// Global-TTL controls for concurrent stores that have one.
///
/// Mirrors the single-owner [`CacheTtl`] trait but with `&self` methods, since concurrent stores
/// are internally synchronized and held behind an `Arc`/`static`. Only the ttl-capable concurrent
/// stores implement it: the sharded TTL stores (`ShardedTtlCache`, `ShardedLruTtlCache`),
/// `RedisCache`, `AsyncRedisCache`, and `RedbCache`. Non-ttl concurrent stores
/// (`ShardedUnboundCache`, `ShardedLruCache`, `ShardedExpiringCache`, `ShardedExpiringLruCache`)
/// deliberately do **not** implement it — they have no global TTL knob, so `set_ttl` simply does
/// not exist on them.
///
/// Refresh-on-hit is **not** part of this trait; it lives on [`ConcurrentCacheRefreshOnHit`],
/// mirroring the [`CacheTtl`] / [`CacheRefreshOnHit`] split on the single-owner side.
pub trait ConcurrentCacheTtl {
    /// Return the ttl of cached values (time to eviction).
    #[must_use]
    fn ttl(&self) -> Option<Duration>;

    /// Set the ttl of cached values, returning the previous value.
    ///
    /// Takes `&self`: concurrent stores are internally synchronized, so this is callable
    /// through a shared reference.
    ///
    /// A zero `Duration` disables expiry — it is exactly equivalent to
    /// [`unset_ttl`](Self::unset_ttl), and subsequently inserted entries never expire.
    /// For the Redis stores this writes keys without any expiry (a plain `SET`). Use
    /// [`try_set_ttl`](Self::try_set_ttl) if you want a zero `ttl` rejected instead.
    fn set_ttl(&self, ttl: Duration) -> Option<Duration>;

    /// Validated variant of [`set_ttl`](Self::set_ttl): returns [`SetTtlError::ZeroTtl`]
    /// when `ttl` is zero instead of storing it. This is the strict "give me a real ttl"
    /// path; to disable expiry, call [`set_ttl`](Self::set_ttl) with a zero `Duration` or
    /// [`unset_ttl`](Self::unset_ttl).
    ///
    /// # Errors
    ///
    /// Returns [`SetTtlError::ZeroTtl`] if `ttl` is zero.
    fn try_set_ttl(&self, ttl: Duration) -> Result<Option<Duration>, crate::SetTtlError> {
        if ttl.is_zero() {
            return Err(crate::SetTtlError::ZeroTtl);
        }
        Ok(self.set_ttl(ttl))
    }

    /// Remove the ttl for cached values, returning the previous value. Equivalent to
    /// [`set_ttl`](Self::set_ttl) with a zero `Duration`.
    ///
    /// For cache implementations that don't support retaining values indefinitely, this method is
    /// a no-op. Takes `&self`: concurrent stores are internally synchronized, so this is
    /// callable through a shared reference.
    fn unset_ttl(&self) -> Option<Duration>;
}

/// Refresh-on-hit control for concurrent stores that have a global TTL.
///
/// The `&self` mirror of [`CacheRefreshOnHit`], split out of [`ConcurrentCacheTtl`] for the same
/// reason: a bound that names the capability should imply the capability. Implemented by
/// `RedisCache`, `AsyncRedisCache`, `RedbCache`, `ShardedTtlCache`, and `ShardedLruTtlCache`:
/// every concurrent store that implements [`ConcurrentCacheTtl`].
///
/// The split discriminates nothing on the concurrent side today: the two implementor sets are
/// currently identical. It exists so the two trait families stay symmetric, so that generic code
/// written against one side ports to the other without a bound changing meaning, and so that a
/// future concurrent TTL store which cannot refresh on hit (the position `TtlSortedCache` occupies
/// among the single-owner stores) can implement [`ConcurrentCacheTtl`] alone rather than stubbing
/// a setter that ignores its argument.
///
/// The trait is not feature-gated; only the built-in implementations are gated on the feature
/// enabling their store.
pub trait ConcurrentCacheRefreshOnHit {
    /// Return `true` if cache hits refresh the ttl of the accessed entry.
    #[must_use]
    fn refresh_on_hit(&self) -> bool;

    /// Set whether cache hits refresh the ttl of cached values, returning the previous flag value.
    ///
    /// Takes `&self`: concurrent stores are internally synchronized (sharded stores use an
    /// `AtomicBool`; `RedisCache` / `RedbCache` use interior mutability), so this is callable
    /// through a shared reference such as an `Arc` or a `LazyLock` static.
    ///
    /// Every implementor honors this: the returned flag is the state the store was actually in
    /// before the call, and the new state takes effect for subsequent hits.
    fn set_refresh_on_hit(&self, refresh: bool) -> bool;
}

/// Capacity resize for concurrent stores that carry a size bound.
///
/// The `&self` mirror of [`CacheSetMaxSize`], taking a shared reference because concurrent stores
/// are internally synchronized and are held behind an `Arc` or a `static`. Implemented by the
/// three bounded sharded stores: `ShardedLruCache`, `ShardedLruTtlCache`, and
/// `ShardedExpiringLruCache`.
///
/// `ShardedUnboundCache`, `ShardedTtlCache`, and `ShardedExpiringCache` do **not** implement it,
/// for the same reason their single-owner counterparts do not: there is no live bound to change.
/// `RedisCache`, `AsyncRedisCache`, and `RedbCache` have no client-side capacity at all.
///
/// The requested bound is ceiling-divided across shards, with a floor of 16 entries per shard
/// when the store has more than one shard, so the effective total can exceed what was asked for:
/// `set_max_size(4)` on a 16-shard cache leaves an effective bound of 256, not 4. A single-shard
/// store has no such floor and resizes to exactly the requested total. The returned
/// "previous bound" is the previous **effective total** (the value `capacity()` would have
/// reported), not the value previously passed to `set_max_size` or `try_set_max_size`, and it is
/// always `Some` on all three sharded implementors — none of them can be built unbounded.
///
/// The resize is **not atomic across shards**: shards are updated one at a time, so two
/// concurrent resizes can interleave their per-shard writes and leave a blend of both requested
/// totals rather than either one cleanly.
///
/// This is an asymmetry with [`CacheSetMaxSize`], which is exact on `LruCache`. Generic code that
/// moves a bound between the single-owner and concurrent traits can silently retain far more than
/// requested: the `set_max_size(4)`-on-16-shards example above resolves to 256 entries, 64x the
/// requested bound.
///
/// The sharded stores divide the bound across shards, so unlike the single-owner side they have a
/// second failure mode: a `max_size` close enough to `usize::MAX` that dividing and multiplying
/// back overflows. [`set_max_size`](Self::set_max_size) panics on it and
/// [`try_set_max_size`](Self::try_set_max_size) reports
/// [`SetMaxSizeError::CapacityOverflow`].
///
/// The trait is not feature-gated; only the built-in impls whose store requires a feature are.
pub trait ConcurrentCacheSetMaxSize {
    /// Change the maximum number of entries, returning the previous bound.
    ///
    /// Takes `&self`: concurrent stores are internally synchronized, so this is callable through a
    /// shared reference. Shrinking below the current entry count evicts eagerly, before this
    /// returns, firing `on_evict` and counting an eviction for each removed entry.
    ///
    /// Both the effective bound this sets and the previous-bound value it returns are the
    /// per-shard-rounded totals described on the trait, not the raw number passed in or
    /// previously requested — see the trait docs for the rounding rule.
    ///
    /// # Panics
    ///
    /// Panics if `max_size` is 0, and panics if splitting `max_size` across the shard count
    /// overflows (a value close enough to `usize::MAX` that dividing by the shard count and
    /// multiplying back does not fit). Generic code that forwards a configuration value or an
    /// admin-API argument straight into this method therefore panics on input it did not
    /// validate, with no concrete store type to consult: use
    /// [`try_set_max_size`](Self::try_set_max_size), which reports the same two conditions as
    /// [`SetMaxSizeError::ZeroMaxSize`] and [`SetMaxSizeError::CapacityOverflow`]. Both panics
    /// fire before any shard lock is taken, so no shard is left resized and no capacity state is
    /// torn.
    fn set_max_size(&self, max_size: usize) -> Option<usize>;

    /// Validated variant of [`set_max_size`](Self::set_max_size): rejects an invalid `max_size`
    /// instead of panicking. On success it is [`set_max_size`](Self::set_max_size), same return,
    /// same rounding of both the effective bound and the returned previous value, and same eager
    /// eviction on shrink.
    ///
    /// # Errors
    ///
    /// Returns [`SetMaxSizeError::ZeroMaxSize`] if `max_size` is 0, and
    /// [`SetMaxSizeError::CapacityOverflow`] if splitting `max_size` across the shard count
    /// overflows.
    fn try_set_max_size(&self, max_size: usize) -> Result<Option<usize>, crate::SetMaxSizeError>;
}

/// Clearing a concurrent store with `on_evict` notification.
///
/// The `&self` mirror of [`CacheClearWithOnEvict`]. [`ConcurrentCached::cache_clear`] removes
/// every entry silently; this fires `on_evict` for each removed entry and, on the stores that
/// track evictions at all, counts it as an eviction. Implemented by all six sharded stores
/// (`ShardedUnboundCache`, `ShardedLruCache`, `ShardedTtlCache`, `ShardedLruTtlCache`,
/// `ShardedExpiringCache`, `ShardedExpiringLruCache`); the Redis and redb stores have no
/// `on_evict` mechanism and implement neither trait in the pair. `ShardedUnboundCache` is the one
/// exception to the eviction count: it has no evictions counter at all, so `metrics().evictions`
/// stays `None` even though `on_evict` still fires for every removed entry.
///
/// The sharded stores also expose the method inherently, and inherent methods take call-site
/// priority, so this trait changes nothing for code naming a concrete sharded type. It adds the
/// route through a `T: ConcurrentCacheClearWithOnEvict` bound.
///
/// The trait is not feature-gated; only the built-in impls whose store requires a feature are.
pub trait ConcurrentCacheClearWithOnEvict {
    /// Remove every entry, firing `on_evict` for each removed entry and counting it as an
    /// eviction.
    ///
    /// The eviction count does not depend on whether an `on_evict` callback is configured, on the
    /// stores that track evictions at all (`ShardedUnboundCache` has no evictions counter).
    ///
    /// Takes `&self`: concurrent stores are internally synchronized, so this is callable through a
    /// shared reference. Clearing is per shard and is not atomic across shards. Contrast
    /// [`ConcurrentCached::cache_clear`], which removes the same entries with neither side effect.
    fn cache_clear_with_on_evict(&self);
}

/// Side-effect-free cache lookup for concurrent stores that can expose a value cheaply.
///
/// This is the concurrent analogue of the single-owner [`CachedPeek`] trait: a genuinely
/// side-effect-free read through a shared (`&self`) reference. A peek does **not** update
/// recency / LRU promotion, does **not** refresh any TTL, does **not** increment hit/miss
/// metrics, and does **not** lazily remove expired entries. An entry whose TTL or per-value
/// expiry has elapsed reads as `None` (it is left in place for a later `cache_get`, an
/// explicit removal, or an `evict()` to reap).
///
/// Unlike [`CachedPeek`], which returns a borrowed `&V`, this trait returns an owned
/// `Option<V>`: on the sharded stores the value lives behind a per-shard lock, so a peek
/// clones it out rather than lending a reference across the lock boundary.
///
/// Only stores that can satisfy these guarantees cheaply implement it: the six in-memory
/// sharded stores (`ShardedUnboundCache`, `ShardedLruCache`, `ShardedTtlCache`,
/// `ShardedLruTtlCache`, `ShardedExpiringCache`, `ShardedExpiringLruCache`). The IO stores
/// (`RedisCache` / `RedbCache` / `AsyncRedisCache`) deliberately do **not** implement it — a
/// server-side or on-disk read cannot promise the no-metrics / no-expiry-sweep semantics
/// without an extra round trip, so peeking is not offered for them.
///
/// This trait is **not** feature-gated (like [`ConcurrentCacheEvict`] and
/// [`ConcurrentCacheTtl`]); only the built-in stores implementing it for expiry-capable
/// variants require `time_stores`.
pub trait ConcurrentCachePeek<K, V>: ConcurrentCacheBase {
    /// Retrieve a clone of the cached value for `k` without any observable side effect.
    ///
    /// No recency/LRU promotion, no TTL refresh, no hit/miss metrics, and no lazy removal of
    /// expired entries; an expired entry reads as `None`.
    ///
    /// # Errors
    ///
    /// Should return `Self::Error` if the operation fails. The sharded implementors are
    /// infallible (`Self::Error = Infallible`), so the outer `Result` is always `Ok`.
    #[must_use = "cache_peek returns the peeked value; ignoring it discards the result"]
    fn cache_peek(&self, k: &K) -> Result<Option<V>, Self::Error>;

    /// Ergonomic alias for [`cache_peek`](Self::cache_peek), mirroring
    /// [`CachedPeek::peek`].
    ///
    /// On the concrete sharded types the inherent `peek(&self, &K) -> Option<V>` takes
    /// call-site priority (like the other inherent shims such as `get`/`set`), so this trait
    /// spelling is intended for generic code over a `ConcurrentCachePeek` bound; use
    /// fully-qualified syntax (`ConcurrentCachePeek::peek(&store, &k)`) to reach it on a
    /// concrete store.
    ///
    /// # Errors
    ///
    /// Should return `Self::Error` if the operation fails.
    fn peek(&self, k: &K) -> Result<Option<V>, Self::Error> {
        self.cache_peek(k)
    }
}

/// Async mirror of [`ConcurrentCachePeek`]: a side-effect-free cache lookup for concurrent
/// stores that can expose a value cheaply.
///
/// [`async_cache_peek`](Self::async_cache_peek) carries the identical contract to the
/// synchronous [`cache_peek`](ConcurrentCachePeek::cache_peek): no recency / LRU promotion,
/// no TTL refresh, no hit/miss metrics accounting, and no lazy removal of expired entries. An
/// entry whose TTL or per-value expiry has elapsed reads as `None` and is left in place for a
/// later `async_cache_get`, an explicit removal, or an `evict()` to reap.
///
/// **`async_cache_peek` is a required method with no default body, deliberately.** A default
/// body could only be written in terms of an ordinary `async_cache_get`, which violates every
/// guarantee above — it promotes on an LRU store, extends the entry's life on a
/// refresh-on-hit TTL store, and moves the hit/miss counters on any store. Generic code bounded
/// on this trait would then be able to rely on none of the contract, leaving the bound
/// decorative. Requiring the method means satisfying the bound implies the behavior.
///
/// Implemented by exactly the six in-memory sharded stores (`ShardedUnboundCache`,
/// `ShardedLruCache`, `ShardedTtlCache`, `ShardedLruTtlCache`, `ShardedExpiringCache`,
/// `ShardedExpiringLruCache`), each with `Self::Error = Infallible`, delegating to its
/// side-effect-free sync `cache_peek`. Those stores never block on IO, so there is nothing to
/// await; the async method exists so generic *async* code can name a peek bound.
///
/// The IO stores (`RedisCache`, `RedbCache`, `AsyncRedisCache`) deliberately implement neither
/// peek trait. Peek is an in-memory concept: an IO-backed store has no client-side recency or
/// TTL state to skip, so every negative guarantee is vacuous, while the cost is still a full
/// network round trip or a disk read transaction. Implementing it would let generic code
/// written for a cheap, side-effect-free read silently become IO-bound.
///
/// The alias [`async_peek`](Self::async_peek) keeps the `async_` prefix rather than being a
/// bare `peek`, so it does not collide with the synchronous inherent
/// `peek(&self, &K) -> Option<V>` the six sharded concrete types already expose.
///
/// This trait is **not** gated on `time_stores` (like [`ConcurrentCachePeek`]); only the
/// built-in expiry-capable implementors require it.
#[cfg(feature = "async_core")]
#[cfg_attr(docsrs, doc(cfg(feature = "async_core")))]
pub trait ConcurrentCachePeekAsync<K, V>: ConcurrentCacheBase {
    /// Retrieve a clone of the cached value for `k` without any observable side effect.
    ///
    /// No recency/LRU promotion, no TTL refresh, no hit/miss metrics, and no lazy removal of
    /// expired entries; an expired entry reads as `None`.
    ///
    /// There is intentionally **no default body**: see the trait docs.
    ///
    /// # Errors
    ///
    /// Should return `Self::Error` if the operation fails. The sharded implementors are
    /// infallible (`Self::Error = Infallible`), so the outer `Result` is always `Ok`.
    #[must_use = "async_cache_peek returns the peeked value; ignoring it discards the result"]
    #[doc(alias = "cache_peek")]
    #[doc(alias = "async_peek")]
    fn async_cache_peek(
        &self,
        k: &K,
    ) -> impl Future<Output = Result<Option<V>, Self::Error>> + Send;

    /// Ergonomic alias for [`async_cache_peek`](Self::async_cache_peek), mirroring
    /// [`ConcurrentCachePeek::peek`].
    ///
    /// Named `async_peek` rather than `peek` so it does not collide with the synchronous
    /// inherent `peek(&self, &K) -> Option<V>` on the concrete sharded types.
    ///
    /// # Errors
    ///
    /// Should return `Self::Error` if the operation fails.
    #[must_use = "async_peek returns the peeked value; ignoring it discards the result"]
    fn async_peek(&self, k: &K) -> impl Future<Output = Result<Option<V>, Self::Error>> + Send
    where
        Self: Sync,
        K: Sync,
    {
        async move { self.async_cache_peek(k).await }
    }
}

/// Cache operations on a store that manages its own synchronization (a shared,
/// `&self` API with owned return values and a fallible `Error`). Implemented by the
/// six in-memory sharded stores (`ShardedUnboundCache`, `ShardedLruCache`,
/// `ShardedTtlCache`, `ShardedLruTtlCache`, `ShardedExpiringCache`,
/// `ShardedExpiringLruCache`) and by the IO stores `RedisCache`/`RedbCache`;
/// implement it directly for a custom concurrent or IO-backed store (this is the
/// ~10-line pattern the 1.0 migration guide recommends in place of the removed
/// `InMemoryAdapter`):
///
/// ```rust
/// use cached::{ConcurrentCacheBase, ConcurrentCached, ConcurrentCachedExt};
/// use std::collections::HashMap;
/// use std::sync::Mutex;
///
/// struct MyStore(Mutex<HashMap<String, u32>>);
///
/// impl ConcurrentCacheBase for MyStore {
///     type Error = std::convert::Infallible;
/// }
///
/// impl ConcurrentCached<String, u32> for MyStore {
///     fn cache_get(&self, k: &String) -> Result<Option<u32>, Self::Error> {
///         Ok(self.0.lock().unwrap().get(k).copied())
///     }
///     fn cache_set(&self, k: String, v: u32) -> Result<Option<u32>, Self::Error> {
///         Ok(self.0.lock().unwrap().insert(k, v))
///     }
///     fn cache_remove(&self, k: &String) -> Result<Option<u32>, Self::Error> {
///         Ok(self.0.lock().unwrap().remove(k))
///     }
///     fn cache_remove_entry(&self, k: &String) -> Result<Option<(String, u32)>, Self::Error> {
///         Ok(self.0.lock().unwrap().remove_entry(k))
///     }
///     fn cache_clear(&self) -> Result<(), Self::Error> {
///         self.0.lock().unwrap().clear();
///         Ok(())
///     }
///     fn cache_reset(&self) -> Result<(), Self::Error> {
///         self.0.lock().unwrap().clear();
///         Ok(())
///     }
///     fn cache_contains(&self, k: &String) -> Result<bool, Self::Error> {
///         Ok(self.0.lock().unwrap().contains_key(k))
///     }
/// }
///
/// let store = MyStore(Mutex::new(HashMap::new()));
/// assert_eq!(store.get(&"k".to_string()).expect("MyStore is infallible"), None);
/// assert_eq!(store.set("k".to_string(), 7).expect("MyStore is infallible"), None);
/// assert_eq!(store.get(&"k".to_string()).expect("MyStore is infallible"), Some(7));
/// assert_eq!(store.remove(&"k".to_string()).expect("MyStore is infallible"), Some(7));
/// ```
///
/// **Using a custom store with `#[concurrent_cached]`**:
///
/// A custom `ty` always takes the fallible code path, because the macro cannot see your
/// `Error` type at expansion time. The cached function must return `Result<T, E>`, and `E`
/// has to be reachable from the store's `Error`, either through a `From` impl or with
/// `map_error`. A plain or `Option<T>` return is rejected with an error spanned at the
/// return type. This applies to every custom store, not just this one.
///
/// ```rust
/// # #[cfg(feature = "proc_macro")]
/// # {
/// # use cached::{ConcurrentCacheBase, ConcurrentCached};
/// # use cached::macros::concurrent_cached;
/// # use std::collections::HashMap;
/// # use std::sync::Mutex;
/// # struct MyStore(Mutex<HashMap<String, u32>>);
/// # impl ConcurrentCacheBase for MyStore {
/// #     type Error = std::convert::Infallible;
/// # }
/// # impl ConcurrentCached<String, u32> for MyStore {
/// #     fn cache_get(&self, k: &String) -> Result<Option<u32>, Self::Error> {
/// #         Ok(self.0.lock().unwrap().get(k).copied())
/// #     }
/// #     fn cache_set(&self, k: String, v: u32) -> Result<Option<u32>, Self::Error> {
/// #         Ok(self.0.lock().unwrap().insert(k, v))
/// #     }
/// #     fn cache_remove(&self, k: &String) -> Result<Option<u32>, Self::Error> {
/// #         Ok(self.0.lock().unwrap().remove(k))
/// #     }
/// #     fn cache_remove_entry(&self, k: &String) -> Result<Option<(String, u32)>, Self::Error> {
/// #         Ok(self.0.lock().unwrap().remove_entry(k))
/// #     }
/// #     fn cache_clear(&self) -> Result<(), Self::Error> {
/// #         self.0.lock().unwrap().clear();
/// #         Ok(())
/// #     }
/// #     fn cache_reset(&self) -> Result<(), Self::Error> {
/// #         self.0.lock().unwrap().clear();
/// #         Ok(())
/// #     }
/// #     fn cache_contains(&self, k: &String) -> Result<bool, Self::Error> {
/// #         Ok(self.0.lock().unwrap().contains_key(k))
/// #     }
/// # }
/// #[concurrent_cached(
///     ty = "MyStore",
///     create = r#"{ MyStore(Mutex::new(HashMap::new())) }"#,
///     key = "String",
///     convert = r#"{ k.to_string() }"#,
///     map_error = "|e| e"
/// )]
/// fn lookup(k: &str) -> Result<u32, std::convert::Infallible> {
///     Ok(k.len() as u32)
/// }
///
/// assert_eq!(lookup("abcd"), Ok(4));
/// # }
/// ```
///
/// See `examples/moka_custom_store.rs` for the same pattern against a third-party cache,
/// including what a store whose API does not line up method-for-method costs to adapt.
///
/// **Async counterpart**:
///
/// The asynchronous [`ConcurrentCachedAsync`] trait names its core operations with an `async_`
/// prefix (`async_cache_get`, `async_cache_set`, …) so they never collide with these synchronous
/// operations when both traits are imported.
///
/// **Why key-lookup methods take `&K` instead of `&Q` (`Borrow<Q>`)**:
///
/// [`Cached`] uses `Borrow<Q>` for all key-lookup methods (e.g. look up a `String` key with a
/// `&str`). `ConcurrentCached` cannot follow the same pattern, for two reasons that no longer
/// weigh the same on the sharded stores as they do on the trait. For an *arbitrary* shard hasher,
/// a borrowed `&Q` may hash differently from the stored `K`, routing the lookup to the wrong
/// shard; that risk is why the trait keeps `&K`. The six sharded stores answer it on their own
/// terms instead: their **inherent** `get`/`remove`/`remove_entry`/`delete`/`contains`/`peek`
/// take `&Q` where `K: Borrow<Q>`, bounded on [`ShardHasher<Q>`](ShardHasher) so the looked-up
/// form must itself be routable. For a `BuildHasher` that bound is met by the blanket
/// `ShardHasher` impl and agreement with the owned key follows from the `Borrow` contract; a
/// hand-written router that adds a borrowed impl takes on `ShardHasher`'s documented cross-impl
/// consistency requirement, which the compiler cannot check. The trait itself still cannot follow, because its implementor set is wider than
/// the sharded stores: the IO stores (`RedbCache`, `RedisCache`) must serialize the key to perform
/// a lookup, and a generic `&Q` where only `K: Borrow<Q>` carries no serialization guarantee;
/// adding a `Q: Serialize` bound would bleed a serde dependency into every `ConcurrentCached`
/// implementation. All key-lookup methods therefore take `&K` directly at the trait level.
pub trait ConcurrentCached<K, V>: ConcurrentCacheBase {
    /// Attempt to retrieve a cached value
    ///
    /// # Errors
    ///
    /// Should return `Self::Error` if the operation fails
    #[must_use = "cache_get returns the looked-up value; ignoring it discards the result"]
    fn cache_get(&self, k: &K) -> Result<Option<V>, Self::Error>;

    /// Insert a key, value pair and return the previous live value at the key, if any.
    /// A displaced entry that has already expired is not returned: it is filtered to
    /// `None` and delivered to `on_evict` instead, so a returned `Some(v)` is always a
    /// previous value that was live for this operation. When an entry crosses its expiry
    /// while the caller is queued for the shard lock, the TTL stores judge it against the
    /// pre-lock instant (returned as a live `Some`), while the per-value-expiry stores
    /// judge it expired (filtered to `None` and delivered to `on_evict`). On
    /// recency-ordered sharded stores, overwriting an existing key promotes it to
    /// most-recently-used.
    ///
    /// # Errors
    ///
    /// Should return `Self::Error` if the operation fails
    #[must_use = "cache_set returns the previous value; ignoring it discards the displaced entry"]
    fn cache_set(&self, k: K, v: V) -> Result<Option<V>, Self::Error>;

    /// Remove a cached value, returning it if it was both present and still live.
    ///
    /// For stores with expiry, an entry that is present but already expired is still
    /// removed (and fires `on_evict` / counts an eviction), but `None` is returned.
    /// Use [`cache_remove_entry`](ConcurrentCached::cache_remove_entry) when you need
    /// to distinguish "key absent" from "key present but expired".
    ///
    /// # Errors
    ///
    /// Should return `Self::Error` if the operation fails
    #[must_use = "cache_remove returns the previous value; ignoring it discards the displaced entry"]
    fn cache_remove(&self, k: &K) -> Result<Option<V>, Self::Error>;

    /// Remove a cached entry, returning the stored key and value whenever an entry
    /// was physically deleted — including entries that were present but already expired.
    ///
    /// This is the key difference from [`cache_remove`](ConcurrentCached::cache_remove):
    /// - `cache_remove` returns `None` for both "key absent" and "key present but expired"
    /// - `cache_remove_entry` returns `Some((stored_key, value))` whenever anything was
    ///   physically deleted, and `None` only when the key was not in the store at all
    ///
    /// This expired-but-present guarantee holds for the in-memory stores (sharded and
    /// non-sharded) and for `RedbCache`, which enforces TTL client-side and can still read
    /// the stored value of an expired entry. Stores that enforce TTL server-side —
    /// `RedisCache` / `AsyncRedisCache` — cannot read the value of a TTL-expired key and so
    /// return `None` for it, exactly like `cache_remove`; see
    /// ["Note on Redis and external stores"](#note-on-redis-and-external-stores) below.
    ///
    /// Removing any present entry fires the store's `on_evict` callback (if set) and,
    /// for stores that track evictions, increments the `evictions` metric.
    ///
    /// # Note on `K: Clone`
    ///
    /// Implementations that reconstruct the stored key from the lookup key — such as
    /// `RedbCache` and `RedisCache` — require `K: Clone` to produce the stored-key half
    /// of the returned tuple.  Sharded in-memory stores return the physically stored key
    /// and do not impose this bound.
    ///
    /// # Note on Redis and external stores
    ///
    /// Stores that enforce TTL server-side (e.g. `RedisCache`, `AsyncRedisCache`) cannot
    /// retrieve the value of a TTL-expired key — `GET` returns nil once the TTL elapses,
    /// even before the background expiry sweep removes the key.  For such stores,
    /// `cache_remove_entry` behaves identically to `cache_remove` and returns `None` for
    /// server-side-expired entries.  Use [`cache_delete`](ConcurrentCached::cache_delete)
    /// (which issues `DEL` directly) to reliably confirm whether any physical entry was removed.
    ///
    /// # Errors
    ///
    /// Should return `Self::Error` if the operation fails
    ///
    /// # Example
    ///
    /// ```rust
    /// use cached::{ConcurrentCached, ConcurrentCachedExt, ShardedUnboundCache};
    ///
    /// let cache: ShardedUnboundCache<String, u32> = ShardedUnboundCache::builder().build().unwrap();
    /// cache.set("key".to_string(), 42);
    ///
    /// // remove_entry always returns Some when the key was present.
    /// let entry = cache.remove_entry(&"key".to_string());
    /// assert_eq!(entry, Some(("key".to_string(), 42)));
    ///
    /// // Returns None only when the key was never present.
    /// assert_eq!(cache.remove_entry(&"missing".to_string()), None);
    /// ```
    #[must_use = "cache_remove_entry returns the previous entry; ignoring it discards the displaced key and value"]
    fn cache_remove_entry(&self, k: &K) -> Result<Option<(K, V)>, Self::Error>;

    /// Delete a cached value without returning or decoding the stored value.
    ///
    /// Returns `true` if an entry (live or expired) was physically removed from the
    /// store, `false` if the key was not present. This differs from the 1.x
    /// behaviour where `cache_delete` returned `false` for expired entries — use
    /// [`cache_remove`](ConcurrentCached::cache_remove) if you need to distinguish
    /// a live removal from an expired one.
    ///
    /// # Errors
    ///
    /// Should return `Self::Error` if the operation fails
    fn cache_delete(&self, k: &K) -> Result<bool, Self::Error> {
        self.cache_remove_entry(k).map(|removed| removed.is_some())
    }

    /// Return `true` if the cache contains a live value for the given key.
    ///
    /// The built-in sharded in-memory stores implement this peek-based: they take a read lock
    /// and check for presence without cloning the value, updating LRU recency, or recording
    /// hit/miss metrics. The IO stores implementing this trait (`RedisCache`, `RedbCache`)
    /// implement it get-based: they fetch the entry and check whether it is present.
    /// (`AsyncRedisCache` implements the async counterpart,
    /// [`async_cache_contains`](ConcurrentCachedAsync::async_cache_contains).)
    ///
    /// This method is object-safe: it takes no generic parameters and does not require
    /// `Self: Sized`, so it is part of the vtable and callable through
    /// `dyn ConcurrentCached` (e.g. `Box<dyn ConcurrentCached<K, V, Error = _>>`), not only on
    /// the concrete type. The inherent `contains` on the sharded stores remains available for
    /// the same check without a trait import.
    ///
    /// # Errors
    ///
    /// Should return `Self::Error` if the operation fails.
    fn cache_contains(&self, k: &K) -> Result<bool, Self::Error>;

    /// Remove all cached entries while preserving capacity allocation and metrics.
    ///
    /// This is a required method. The concurrent analogue of [`Cached::cache_clear`].
    /// The internally-synchronized sharded in-memory stores clear every shard; `RedbCache`
    /// clears its (local, single-file) redb table; and `RedisCache` / `AsyncRedisCache`
    /// use a namespace-scoped `SCAN` + batched `DEL` (O(n) in matching keys and not atomic;
    /// see the store docs). To also reset metrics, call
    /// [`cache_reset_metrics`](ConcurrentCached::cache_reset_metrics), or use
    /// [`cache_reset`](ConcurrentCached::cache_reset) to do both at once.
    ///
    /// # Errors
    ///
    /// Should return `Self::Error` if the operation fails.
    fn cache_clear(&self) -> Result<(), Self::Error>;

    /// Reset all entries and metrics (hits, misses, evictions) to zero.
    ///
    /// This is a required method. The concurrent analogue of [`Cached::cache_reset`]. Store
    /// configuration (capacity, TTL, `on_evict` callbacks) is preserved. The sharded in-memory
    /// stores clear every shard and zero their metrics; `RedbCache` and `RedisCache` /
    /// `AsyncRedisCache` clear their entries (they track no in-memory metrics, so resetting
    /// is exactly [`cache_clear`](ConcurrentCached::cache_clear)).
    /// To reset entries without resetting metrics, use
    /// [`cache_clear`](ConcurrentCached::cache_clear).
    ///
    /// # Errors
    ///
    /// Should return `Self::Error` if the operation fails.
    fn cache_reset(&self) -> Result<(), Self::Error>;

    /// Reset hit/miss/eviction counters to zero without removing entries.
    ///
    /// The concurrent analogue of [`Cached::cache_reset_metrics`]. The default is a **no-op**;
    /// the sharded in-memory stores override it to zero their per-shard metrics.
    ///
    /// # Errors
    ///
    /// Should return `Self::Error` if the operation fails.
    fn cache_reset_metrics(&self) -> Result<(), Self::Error> {
        Ok(())
    }

    /// Return the cached value for `k`, or compute `f()`, store it, and return it.
    ///
    /// This is a non-atomic get-then-set: on a miss, another thread may store a value
    /// for the same key between the get and the set, in which case the computed value
    /// overwrites the concurrent write. `#[concurrent_cached]` has no `sync_writes`
    /// equivalent; if you need the body to run at most once per key, serialize the call
    /// yourself (for example behind your own per-key lock).
    ///
    /// # Errors
    ///
    /// Returns `Self::Error` if `cache_get` or `cache_set` fails.
    ///
    /// The `where Self: Sized` bound keeps this generic method out of the vtable so
    /// `ConcurrentCached` stays dyn-compatible. The associated `Error` lives on the
    /// [`ConcurrentCacheBase`] supertrait, so the usable trait-object type is
    /// `dyn ConcurrentCached<K, V, Error = E>`. Call this method
    /// through a concrete type or the [`ConcurrentCachedExt`] alias.
    fn cache_get_or_set_with<F: FnOnce() -> V>(&self, k: K, f: F) -> Result<V, Self::Error>
    where
        Self: Sized,
        V: Clone,
    {
        if let Some(v) = self.cache_get(&k)? {
            return Ok(v);
        }
        let v = f();
        self.cache_set(k, v.clone())?;
        Ok(v)
    }

    /// Return the cached value for `k`, or compute `f()`, store it on success, and
    /// return it. Fallible-init counterpart of
    /// [`cache_get_or_set_with`](ConcurrentCached::cache_get_or_set_with): if `f()`
    /// returns `Err`, nothing is stored and the error is returned in the inner
    /// `Result`.
    ///
    /// The two error sources stay separate: the outer `Result` carries the store's
    /// `Self::Error` (from `cache_get` / `cache_set`), the inner `Result` carries the
    /// closure's `E`. For the in-memory sharded stores the outer error is
    /// [`Infallible`](std::convert::Infallible), so `.unwrap()` on the outer layer is
    /// safe, matching the other core methods.
    ///
    /// Same non-atomic get-then-set caveat as
    /// [`cache_get_or_set_with`](ConcurrentCached::cache_get_or_set_with); the
    /// `where Self: Sized` bound likewise keeps this generic method out of the vtable.
    fn cache_try_get_or_set_with<F: FnOnce() -> Result<V, E>, E>(
        &self,
        k: K,
        f: F,
    ) -> Result<Result<V, E>, Self::Error>
    where
        Self: Sized,
        V: Clone,
    {
        if let Some(v) = self.cache_get(&k)? {
            return Ok(Ok(v));
        }
        match f() {
            Ok(v) => {
                self.cache_set(k, v.clone())?;
                Ok(Ok(v))
            }
            Err(e) => Ok(Err(e)),
        }
    }
}

/// Short-alias extension for [`ConcurrentCached`] stores.
///
/// Every type that implements `ConcurrentCached<K, V>` automatically implements this trait
/// through a blanket impl. The methods here are ergonomic short names that delegate to the
/// `cache_`-prefixed core methods on `ConcurrentCached`. Import this trait (or
/// `use cached::prelude::*;`) to bring the short names into scope:
///
/// ```rust
/// use cached::{ConcurrentCachedExt, ShardedUnboundCache};
///
/// let cache: ShardedUnboundCache<String, u32> = ShardedUnboundCache::builder().build().unwrap();
/// // Inherent get/set take priority over trait methods on sharded stores;
/// // use fully-qualified syntax for the trait version.
/// ConcurrentCachedExt::set(&cache, "key".to_string(), 42).unwrap();
/// assert_eq!(ConcurrentCachedExt::get(&cache, &"key".to_string()).unwrap(), Some(42));
/// ConcurrentCachedExt::remove(&cache, &"key".to_string()).unwrap();
/// ```
///
/// Note: for the sharded in-memory stores, the inherent `get`/`set`/`remove`/`delete` methods
/// (returning unwrapped `Option`/`bool` rather than `Result`) take priority at the call site
/// over these trait methods. Use fully-qualified syntax
/// (`ConcurrentCachedExt::get(&store, &k)`) or the `cache_`-prefixed form
/// (`ConcurrentCached::cache_get`) to explicitly call the trait version.
pub trait ConcurrentCachedExt<K, V>: ConcurrentCached<K, V> {
    /// Retrieve a cached value. Delegates to [`cache_get`](ConcurrentCached::cache_get).
    #[must_use = "get returns the looked-up value; ignoring it discards the result"]
    fn get(&self, k: &K) -> Result<Option<V>, Self::Error>;

    /// Insert a key-value pair and return the previous value. Delegates to
    /// [`cache_set`](ConcurrentCached::cache_set).
    #[must_use = "set returns the previous value; ignoring it discards the displaced entry"]
    fn set(&self, k: K, v: V) -> Result<Option<V>, Self::Error>;

    /// Remove a cached value and return it. Delegates to
    /// [`cache_remove`](ConcurrentCached::cache_remove).
    #[must_use = "remove returns the previous value; ignoring it discards the displaced entry"]
    fn remove(&self, k: &K) -> Result<Option<V>, Self::Error>;

    /// Remove a cached entry and return the stored key and value. Delegates to
    /// [`cache_remove_entry`](ConcurrentCached::cache_remove_entry).
    #[must_use = "remove_entry returns the previous entry; ignoring it discards the displaced key and value"]
    fn remove_entry(&self, k: &K) -> Result<Option<(K, V)>, Self::Error>;

    /// Delete a cached value without returning it. Delegates to
    /// [`cache_delete`](ConcurrentCached::cache_delete).
    fn delete(&self, k: &K) -> Result<bool, Self::Error>;

    /// Remove all entries from the cache. Delegates to
    /// [`cache_clear`](ConcurrentCached::cache_clear).
    fn clear(&self) -> Result<(), Self::Error>;

    /// Remove all entries and reset the cache to its initial state. Delegates to
    /// [`cache_reset`](ConcurrentCached::cache_reset).
    fn reset(&self) -> Result<(), Self::Error>;

    /// Return `true` if the cache contains a live value for the given key. Delegates to
    /// [`cache_contains`](ConcurrentCached::cache_contains).
    fn contains(&self, k: &K) -> Result<bool, Self::Error>;

    /// Return the cached value for `k`, or compute and store `f()`. Delegates to
    /// [`cache_get_or_set_with`](ConcurrentCached::cache_get_or_set_with).
    fn get_or_set_with<F: FnOnce() -> V>(&self, k: K, f: F) -> Result<V, Self::Error>
    where
        V: Clone;

    /// Return the cached value for `k`, or compute and store `f()`, propagating a closure
    /// failure. Delegates to
    /// [`cache_try_get_or_set_with`](ConcurrentCached::cache_try_get_or_set_with). The outer
    /// `Result` carries the store's `Self::Error`; the inner `Result` carries the closure's
    /// `E` (on `Err` nothing is stored).
    fn try_get_or_set_with<F: FnOnce() -> Result<V, E>, E>(
        &self,
        k: K,
        f: F,
    ) -> Result<Result<V, E>, Self::Error>
    where
        V: Clone;

    /// Return the number of entries currently in the cache, if cheaply available.
    /// Delegates to [`cache_size`](ConcurrentCacheBase::cache_size).
    ///
    /// Note: the concrete sharded stores also expose inherent `len`/`is_empty`
    /// returning plain values, which take call-site priority over these aliases.
    fn len(&self) -> Result<Option<usize>, Self::Error>;

    /// Return `true` if the cache contains no entries, if cheaply available.
    /// Delegates to [`cache_is_empty`](ConcurrentCacheBase::cache_is_empty).
    fn is_empty(&self) -> Result<Option<bool>, Self::Error>;

    /// Return the number of cache hits, if tracked. Delegates to
    /// [`cache_hits`](ConcurrentCacheBase::cache_hits).
    #[must_use]
    fn hits(&self) -> Option<u64>;

    /// Return the number of cache misses, if tracked. Delegates to
    /// [`cache_misses`](ConcurrentCacheBase::cache_misses).
    #[must_use]
    fn misses(&self) -> Option<u64>;

    /// Return the cache's capacity bound, if it has one. Delegates to
    /// [`cache_capacity`](ConcurrentCacheBase::cache_capacity).
    ///
    /// Note: the always-bounded sharded LRU-family stores (`ShardedLruCache`,
    /// `ShardedLruTtlCache`, `ShardedExpiringLruCache`) also expose an inherent
    /// `capacity` returning a plain `usize`, which takes call-site priority over
    /// this alias on those concrete types; the alias applies in generic
    /// `ConcurrentCached`-bounded code.
    #[must_use]
    fn capacity(&self) -> Option<usize>;

    /// Return the number of evictions, if tracked. Delegates to
    /// [`cache_evictions`](ConcurrentCacheBase::cache_evictions).
    #[must_use]
    fn evictions(&self) -> Option<u64>;
}

impl<K, V, T: ConcurrentCached<K, V>> ConcurrentCachedExt<K, V> for T {
    fn get(&self, k: &K) -> Result<Option<V>, Self::Error> {
        self.cache_get(k)
    }

    fn set(&self, k: K, v: V) -> Result<Option<V>, Self::Error> {
        self.cache_set(k, v)
    }

    fn remove(&self, k: &K) -> Result<Option<V>, Self::Error> {
        self.cache_remove(k)
    }

    fn remove_entry(&self, k: &K) -> Result<Option<(K, V)>, Self::Error> {
        self.cache_remove_entry(k)
    }

    fn delete(&self, k: &K) -> Result<bool, Self::Error> {
        self.cache_delete(k)
    }

    fn clear(&self) -> Result<(), Self::Error> {
        self.cache_clear()
    }

    fn reset(&self) -> Result<(), Self::Error> {
        self.cache_reset()
    }

    fn contains(&self, k: &K) -> Result<bool, Self::Error> {
        self.cache_contains(k)
    }

    fn get_or_set_with<F: FnOnce() -> V>(&self, k: K, f: F) -> Result<V, Self::Error>
    where
        V: Clone,
    {
        self.cache_get_or_set_with(k, f)
    }

    fn try_get_or_set_with<F: FnOnce() -> Result<V, E>, E>(
        &self,
        k: K,
        f: F,
    ) -> Result<Result<V, E>, Self::Error>
    where
        V: Clone,
    {
        self.cache_try_get_or_set_with(k, f)
    }

    fn len(&self) -> Result<Option<usize>, Self::Error> {
        self.cache_size()
    }

    fn is_empty(&self) -> Result<Option<bool>, Self::Error> {
        self.cache_is_empty()
    }

    fn hits(&self) -> Option<u64> {
        self.cache_hits()
    }

    fn misses(&self) -> Option<u64> {
        self.cache_misses()
    }

    fn capacity(&self) -> Option<usize> {
        self.cache_capacity()
    }

    fn evictions(&self) -> Option<u64> {
        self.cache_evictions()
    }
}

/// **Direct-call syntax warning**:
///
/// `ConcurrentCachedAsync` names its core operations with an `async_` prefix
/// (`async_cache_get`, `async_cache_set`, `async_cache_remove`, `async_cache_remove_entry`,
/// `async_cache_delete`) so they never collide with the synchronous [`ConcurrentCached`]
/// operations (`cache_get`, `cache_set`, …) even when both traits are imported. This means
/// method-call syntax on a store that implements both traits is unambiguous:
/// ```rust,ignore
/// store.async_cache_get(&key).await
/// ```
///
/// **Short aliases**: the deduplicated aliases live on [`ConcurrentCachedAsyncExt`] and keep the
/// `async_` prefix (`async_get`, `async_set`, ...) rather than being bare `get`/`set`. The
/// `cache_`-prefixed names on this trait remain the canonical spelling.
///
/// **Custom `!Sync` implementors**: `async_cache_clear`, `async_cache_reset`,
/// `async_cache_delete`, and `async_cache_reset_metrics` each carry a `where Self: Sync`
/// bound so the returned future is `Send`. `async_cache_clear` and `async_cache_reset`
/// are required methods, so every implementor provides them; the `Self: Sync` bound means
/// their futures are only `Send` when the store is `Sync`. `async_cache_delete` and
/// `async_cache_reset_metrics` have default bodies that also require `Self: Sync`; a store
/// that is not `Sync` must override them (even with an identical body) to remove the
/// `Self: Sync` constraint on the returned future.
#[cfg(feature = "async_core")]
#[cfg_attr(docsrs, doc(cfg(feature = "async_core")))]
pub trait ConcurrentCachedAsync<K, V>: ConcurrentCacheBase {
    /// Attempt to retrieve a cached value, returning `None` on a miss.
    ///
    /// The async counterpart of [`ConcurrentCached::cache_get`].
    ///
    /// # Errors
    ///
    /// Should return `Self::Error` if the operation fails.
    #[must_use = "async_cache_get returns the looked-up value; ignoring it discards the result"]
    #[doc(alias = "async_get")]
    #[doc(alias = "cache_get")]
    fn async_cache_get(&self, k: &K)
    -> impl Future<Output = Result<Option<V>, Self::Error>> + Send;

    /// Insert a key, value pair and return the previous value at the key, if any,
    /// without checking expiry (see [`ConcurrentCached::cache_set`] for the expiry
    /// caveat on the returned value).
    ///
    /// The async counterpart of [`ConcurrentCached::cache_set`].
    ///
    /// # Errors
    ///
    /// Should return `Self::Error` if the operation fails.
    #[must_use = "async_cache_set returns the previous value; ignoring it discards the displaced entry"]
    #[doc(alias = "async_set")]
    #[doc(alias = "cache_set")]
    fn async_cache_set(
        &self,
        k: K,
        v: V,
    ) -> impl Future<Output = Result<Option<V>, Self::Error>> + Send;

    /// Remove a cached value, returning it if it was both present and still live.
    #[must_use = "async_cache_remove returns the previous value; ignoring it discards the displaced entry"]
    #[doc(alias = "async_remove")]
    #[doc(alias = "cache_remove")]
    fn async_cache_remove(
        &self,
        k: &K,
    ) -> impl Future<Output = Result<Option<V>, Self::Error>> + Send;

    /// Remove a cached entry, returning the stored key and value whenever an entry
    /// was physically deleted — including entries that were present but already expired.
    #[must_use = "async_cache_remove_entry returns the previous entry; ignoring it discards the displaced key and value"]
    #[doc(alias = "async_remove_entry")]
    #[doc(alias = "cache_remove_entry")]
    fn async_cache_remove_entry(
        &self,
        k: &K,
    ) -> impl Future<Output = Result<Option<(K, V)>, Self::Error>> + Send;

    /// Delete a cached value without returning or decoding the stored value.
    /// Returns `true` if an entry (live or expired) was physically removed from the
    /// store, `false` if the key was not present.
    #[doc(alias = "async_delete")]
    #[doc(alias = "cache_delete")]
    fn async_cache_delete(&self, k: &K) -> impl Future<Output = Result<bool, Self::Error>> + Send
    where
        Self: Sync,
        K: Sync,
    {
        async move { self.async_cache_remove_entry(k).await.map(|r| r.is_some()) }
    }

    /// Return `true` if the cache contains a live value for the given key.
    ///
    /// The built-in sharded in-memory stores implement this peek-based: they take a read lock
    /// and check for presence without cloning the value, updating LRU recency, or recording
    /// hit/miss metrics. The IO stores (`AsyncRedisCache`, `RedbCache`) implement it get-based:
    /// they fetch the entry and check whether it is present.
    ///
    /// # Errors
    ///
    /// Should return `Self::Error` if the operation fails.
    ///
    /// Carries no `Self: Sized` bound, matching the synchronous
    /// [`ConcurrentCached::cache_contains`] (which omits it so the method stays reachable
    /// through a trait object) and the neighbouring
    /// [`async_cache_delete`](Self::async_cache_delete). The bound constrained implementors
    /// without buying anything here, so it is gone.
    #[doc(alias = "cache_contains")]
    fn async_cache_contains(&self, k: &K) -> impl Future<Output = Result<bool, Self::Error>> + Send
    where
        Self: Sync,
        K: Sync;

    /// Remove all cached entries while preserving capacity allocation and metrics.
    ///
    /// This is a required method. The async counterpart of [`ConcurrentCached::cache_clear`].
    /// The internally-synchronized sharded in-memory stores and `RedbCache` clear their entries;
    /// `AsyncRedisCache` uses a namespace-scoped `SCAN` + batched `DEL` (O(n) in matching keys
    /// and not atomic; see the store docs).
    ///
    /// # Errors
    ///
    /// Should return `Self::Error` if the operation fails.
    #[doc(alias = "cache_clear")]
    fn async_cache_clear(&self) -> impl Future<Output = Result<(), Self::Error>> + Send
    where
        Self: Sync;

    /// Reset all entries and metrics (hits, misses, evictions) to zero.
    ///
    /// This is a required method. The async counterpart of [`ConcurrentCached::cache_reset`].
    /// Store configuration (capacity, TTL, `on_evict` callbacks) is preserved. The sharded
    /// in-memory stores clear every shard and zero their metrics; `RedbCache` and
    /// `AsyncRedisCache` clear their entries (they track no in-memory metrics, so resetting
    /// is exactly [`async_cache_clear`](ConcurrentCachedAsync::async_cache_clear)).
    ///
    /// # Errors
    ///
    /// Should return `Self::Error` if the operation fails.
    #[doc(alias = "cache_reset")]
    fn async_cache_reset(&self) -> impl Future<Output = Result<(), Self::Error>> + Send
    where
        Self: Sync;

    /// Reset hit/miss/eviction counters to zero without removing entries.
    ///
    /// The async counterpart of [`ConcurrentCached::cache_reset_metrics`]. The default is a
    /// **no-op** that resolves to `Ok(())`; the sharded in-memory stores override it to zero
    /// their per-shard metrics.
    ///
    /// # Errors
    ///
    /// Should return `Self::Error` if the operation fails.
    #[doc(alias = "cache_reset_metrics")]
    fn async_cache_reset_metrics(&self) -> impl Future<Output = Result<(), Self::Error>> + Send
    where
        Self: Sync,
    {
        async move { Ok(()) }
    }

    /// Return the cached value for `k`, or compute `f()`, store it, and return it.
    ///
    /// This is a non-atomic get-then-set: on a miss, another thread may store a value
    /// for the same key between the get and the set, in which case the computed value
    /// overwrites the concurrent write. `#[concurrent_cached]` has no `sync_writes`
    /// equivalent; if you need the body to run at most once per key, serialize the call
    /// yourself (for example behind your own per-key lock).
    ///
    /// # Errors
    ///
    /// Returns `Self::Error` if `async_cache_get` or `async_cache_set` fails.
    fn async_cache_get_or_set_with<F, Fut>(
        &self,
        k: K,
        f: F,
    ) -> impl Future<Output = Result<V, Self::Error>> + Send
    where
        Self: Sync,
        K: Send + Sync,
        V: Clone + Send,
        F: FnOnce() -> Fut + Send,
        Fut: Future<Output = V> + Send,
    {
        async move {
            if let Some(v) = self.async_cache_get(&k).await? {
                return Ok(v);
            }
            let v = f().await;
            self.async_cache_set(k, v.clone()).await?;
            Ok(v)
        }
    }

    /// Return the cached value for `k`, or compute `f()`, store it on success, and
    /// return it. Fallible-init counterpart of
    /// [`async_cache_get_or_set_with`](ConcurrentCachedAsync::async_cache_get_or_set_with):
    /// if `f()` resolves to `Err`, nothing is stored and the error is returned in the
    /// inner `Result`.
    ///
    /// The two error sources stay separate: the outer `Result` carries the store's
    /// `Self::Error` (from `async_cache_get` / `async_cache_set`), the inner `Result`
    /// carries the closure's `E`, mirroring
    /// [`ConcurrentCached::cache_try_get_or_set_with`].
    ///
    /// Same non-atomic get-then-set caveat as
    /// [`async_cache_get_or_set_with`](ConcurrentCachedAsync::async_cache_get_or_set_with).
    fn async_cache_try_get_or_set_with<F, Fut, E>(
        &self,
        k: K,
        f: F,
    ) -> impl Future<Output = Result<Result<V, E>, Self::Error>> + Send
    where
        Self: Sync,
        K: Send + Sync,
        V: Clone + Send,
        E: Send,
        F: FnOnce() -> Fut + Send,
        Fut: Future<Output = Result<V, E>> + Send,
    {
        async move {
            if let Some(v) = self.async_cache_get(&k).await? {
                return Ok(Ok(v));
            }
            match f().await {
                Ok(v) => {
                    self.async_cache_set(k, v.clone()).await?;
                    Ok(Ok(v))
                }
                Err(e) => Ok(Err(e)),
            }
        }
    }
}

/// Short-alias extension for [`ConcurrentCachedAsync`] stores, the async counterpart of
/// [`ConcurrentCachedExt`].
///
/// Every type that implements `ConcurrentCachedAsync<K, V>` automatically implements this trait
/// through a blanket impl, which is the only implementation: the aliases are required methods
/// filled in by the blanket impl rather than defaulted ones, so a downstream type cannot
/// override an alias and make it disagree with the core method it names. Import this trait (or
/// `use cached::prelude::*;`) to bring the short names into scope.
///
/// **Only genuinely asynchronous operations are aliased here.** The introspection and metric
/// accessors (`cache_size`, `cache_is_empty`, `cache_hits`, `cache_misses`, `cache_capacity`,
/// `cache_evictions`) live on [`ConcurrentCacheBase`], are already callable on any async store
/// without importing an extension trait, and return plain values rather than futures. Prefixing
/// them with `async_` would promise a future that is not there, so they are deliberately absent.
/// The same holds for [`ConcurrentCacheBase::metrics`].
///
/// **The aliases keep the `async_` prefix.** They are `async_get` / `async_set` / ... rather than
/// bare `get` / `set`, because the stores that implement `ConcurrentCachedAsync` also implement
/// the synchronous [`ConcurrentCached`]: the six sharded in-memory stores and `RedbCache` all
/// implement both. Since both extension traits are blanket-implemented and both are in
/// [`prelude`], a bare `get` here would be a second applicable candidate on those types and
/// `store.get(&k)` would become an ambiguity error (E0034); on the sharded types, whose concrete
/// inherent `get` takes call-site priority, the async alias would instead be silently
/// unreachable through method syntax. The prefix is the same device
/// [`ConcurrentCachePeekAsync::async_peek`] uses for the same reason.
///
/// ```rust
/// # #[cfg(feature = "async_core")]
/// # {
/// use cached::{ConcurrentCachedAsyncExt, ShardedUnboundCache};
///
/// futures::executor::block_on(async {
///     let cache: ShardedUnboundCache<String, u32> =
///         ShardedUnboundCache::builder().build().unwrap();
///     assert_eq!(cache.async_set("key".to_string(), 42).await.unwrap(), None);
///     assert_eq!(cache.async_get(&"key".to_string()).await.unwrap(), Some(42));
///     assert_eq!(cache.async_remove(&"key".to_string()).await.unwrap(), Some(42));
/// });
/// # }
/// ```
#[cfg(feature = "async_core")]
#[cfg_attr(docsrs, doc(cfg(feature = "async_core")))]
pub trait ConcurrentCachedAsyncExt<K, V>: ConcurrentCachedAsync<K, V> {
    /// Retrieve a cached value. Delegates to
    /// [`async_cache_get`](ConcurrentCachedAsync::async_cache_get).
    #[must_use = "async_get returns the looked-up value; ignoring it discards the result"]
    fn async_get(&self, k: &K) -> impl Future<Output = Result<Option<V>, Self::Error>> + Send;

    /// Insert a key-value pair and return the previous value. Delegates to
    /// [`async_cache_set`](ConcurrentCachedAsync::async_cache_set).
    #[must_use = "async_set returns the previous value; ignoring it discards the displaced entry"]
    fn async_set(&self, k: K, v: V) -> impl Future<Output = Result<Option<V>, Self::Error>> + Send;

    /// Remove a cached value and return it. Delegates to
    /// [`async_cache_remove`](ConcurrentCachedAsync::async_cache_remove).
    #[must_use = "async_remove returns the previous value; ignoring it discards the displaced entry"]
    fn async_remove(&self, k: &K) -> impl Future<Output = Result<Option<V>, Self::Error>> + Send;

    /// Remove a cached entry and return the stored key and value. Delegates to
    /// [`async_cache_remove_entry`](ConcurrentCachedAsync::async_cache_remove_entry).
    #[must_use = "async_remove_entry returns the previous entry; ignoring it discards the displaced key and value"]
    fn async_remove_entry(
        &self,
        k: &K,
    ) -> impl Future<Output = Result<Option<(K, V)>, Self::Error>> + Send;

    /// Delete a cached value without returning it. Delegates to
    /// [`async_cache_delete`](ConcurrentCachedAsync::async_cache_delete).
    fn async_delete(&self, k: &K) -> impl Future<Output = Result<bool, Self::Error>> + Send
    where
        Self: Sync,
        K: Sync;

    /// Return `true` if the cache contains a live value for the given key. Delegates to
    /// [`async_cache_contains`](ConcurrentCachedAsync::async_cache_contains).
    fn async_contains(&self, k: &K) -> impl Future<Output = Result<bool, Self::Error>> + Send
    where
        Self: Sync,
        K: Sync;

    /// Remove all entries from the cache. Delegates to
    /// [`async_cache_clear`](ConcurrentCachedAsync::async_cache_clear).
    fn async_clear(&self) -> impl Future<Output = Result<(), Self::Error>> + Send
    where
        Self: Sync;

    /// Remove all entries and reset the cache to its initial state. Delegates to
    /// [`async_cache_reset`](ConcurrentCachedAsync::async_cache_reset).
    fn async_reset(&self) -> impl Future<Output = Result<(), Self::Error>> + Send
    where
        Self: Sync;

    /// Return the cached value for `k`, or await `f()`, store it, and return it. Delegates to
    /// [`async_cache_get_or_set_with`](ConcurrentCachedAsync::async_cache_get_or_set_with).
    ///
    /// Inherits that method's non-atomic get-then-set caveat: on a miss another task may store a
    /// value for the same key between the get and the set, and the computed value overwrites it.
    #[must_use = "async_get_or_set_with returns the cached or freshly computed value; ignoring it discards the result"]
    fn async_get_or_set_with<F, Fut>(
        &self,
        k: K,
        f: F,
    ) -> impl Future<Output = Result<V, Self::Error>> + Send
    where
        Self: Sync,
        K: Send + Sync,
        V: Clone + Send,
        F: FnOnce() -> Fut + Send,
        Fut: Future<Output = V> + Send;

    /// Return the cached value for `k`, or await `f()`, store it on success, and return it.
    /// Delegates to
    /// [`async_cache_try_get_or_set_with`](ConcurrentCachedAsync::async_cache_try_get_or_set_with).
    ///
    /// The two error sources stay separate: the outer `Result` carries the store's `Self::Error`,
    /// the inner `Result` carries the closure's `E`. On `Err` nothing is stored.
    #[must_use = "async_try_get_or_set_with returns the cached or freshly computed value; ignoring it discards the result"]
    fn async_try_get_or_set_with<F, Fut, E>(
        &self,
        k: K,
        f: F,
    ) -> impl Future<Output = Result<Result<V, E>, Self::Error>> + Send
    where
        Self: Sync,
        K: Send + Sync,
        V: Clone + Send,
        E: Send,
        F: FnOnce() -> Fut + Send,
        Fut: Future<Output = Result<V, E>> + Send;
}

#[cfg(feature = "async_core")]
impl<K, V, T: ConcurrentCachedAsync<K, V>> ConcurrentCachedAsyncExt<K, V> for T {
    fn async_get(&self, k: &K) -> impl Future<Output = Result<Option<V>, Self::Error>> + Send {
        self.async_cache_get(k)
    }

    fn async_set(&self, k: K, v: V) -> impl Future<Output = Result<Option<V>, Self::Error>> + Send {
        self.async_cache_set(k, v)
    }

    fn async_remove(&self, k: &K) -> impl Future<Output = Result<Option<V>, Self::Error>> + Send {
        self.async_cache_remove(k)
    }

    fn async_remove_entry(
        &self,
        k: &K,
    ) -> impl Future<Output = Result<Option<(K, V)>, Self::Error>> + Send {
        self.async_cache_remove_entry(k)
    }

    fn async_delete(&self, k: &K) -> impl Future<Output = Result<bool, Self::Error>> + Send
    where
        Self: Sync,
        K: Sync,
    {
        self.async_cache_delete(k)
    }

    fn async_contains(&self, k: &K) -> impl Future<Output = Result<bool, Self::Error>> + Send
    where
        Self: Sync,
        K: Sync,
    {
        self.async_cache_contains(k)
    }

    fn async_clear(&self) -> impl Future<Output = Result<(), Self::Error>> + Send
    where
        Self: Sync,
    {
        self.async_cache_clear()
    }

    fn async_reset(&self) -> impl Future<Output = Result<(), Self::Error>> + Send
    where
        Self: Sync,
    {
        self.async_cache_reset()
    }

    fn async_get_or_set_with<F, Fut>(
        &self,
        k: K,
        f: F,
    ) -> impl Future<Output = Result<V, Self::Error>> + Send
    where
        Self: Sync,
        K: Send + Sync,
        V: Clone + Send,
        F: FnOnce() -> Fut + Send,
        Fut: Future<Output = V> + Send,
    {
        self.async_cache_get_or_set_with(k, f)
    }

    fn async_try_get_or_set_with<F, Fut, E>(
        &self,
        k: K,
        f: F,
    ) -> impl Future<Output = Result<Result<V, E>, Self::Error>> + Send
    where
        Self: Sync,
        K: Send + Sync,
        V: Clone + Send,
        E: Send,
        F: FnOnce() -> Fut + Send,
        Fut: Future<Output = Result<V, E>> + Send,
    {
        self.async_cache_try_get_or_set_with(k, f)
    }
}

/// Borrowed-set extension for serialize-based concurrent stores.
///
/// Stores that persist values by serialization (`RedisCache`, `RedbCache`) can accept the
/// key and value by reference, avoiding the clone that
/// [`ConcurrentCached::cache_set`] requires to take ownership.
/// This trait is additive: it does not replace `cache_set`, and the sharded in-memory stores
/// (which store the value directly) do not implement it.
///
/// The `#[concurrent_cached]` macro automatically routes its cache-set through this trait when
/// the concrete store implements it (no opt-in needed), falling back to the owned `cache_set`
/// otherwise.
///
/// Implementing this on a custom `#[concurrent_cached]` store (one supplied via `ty` / `create`)
/// is worthwhile when the store serializes its values: the macro will then set entries from the
/// borrowed `&V` with no clone. Without it, the macro takes the owned fallback, which requires
/// `V: Clone` and clones the value on every set.
pub trait SerializeCached<K, V>: ConcurrentCached<K, V> {
    /// Insert a key/value pair, taking both by reference.
    ///
    /// Semantically equivalent to [`ConcurrentCached::cache_set`] but serializes from the
    /// borrowed `k`/`v` without cloning.
    ///
    /// Unlike [`ConcurrentCached::cache_set`], this does not return the previous value.
    /// On IO stores (redis) that would cost an extra read+decode per write; dropping it
    /// keeps the write path to a single round-trip. If you need the prior value, call
    /// [`ConcurrentCached::cache_get`] yourself before setting.
    ///
    /// # Errors
    ///
    /// Returns `Self::Error` if the operation fails.
    fn cache_set_ref(&self, k: &K, v: &V) -> Result<(), Self::Error>;
}

/// Async borrowed-set extension for serialize-based concurrent stores.
///
/// The async counterpart of [`SerializeCached`], implemented by `AsyncRedisCache` and `RedbCache`.
///
/// The `#[concurrent_cached]` macro routes its async set through `__set_dispatch_async::cache_set_dispatch`,
/// which prefers `async_cache_set_ref` (borrowed, no clone) when `S: SerializeCachedAsync + Sync`
/// and `V: Sync`, and falls back to owned `async_cache_set` (cloning `V`) otherwise. A custom
/// `Send + !Sync` async store that implements this trait still takes the clone fallback (requiring
/// `V: Clone`); a `Send + !Sync + !Clone` value cannot take either route and fails to compile.
/// That failure surfaces as a trait-resolution error at the generated set site (referring to
/// `__set_dispatch_async` helpers), not at your `impl`; add a `V: Sync` or `V: Clone` bound
/// to resolve it.
#[cfg(feature = "async_core")]
#[cfg_attr(docsrs, doc(cfg(feature = "async_core")))]
pub trait SerializeCachedAsync<K, V>: ConcurrentCachedAsync<K, V> {
    /// Insert a key/value pair, taking both by reference.
    ///
    /// The async counterpart of [`SerializeCached::cache_set_ref`].
    ///
    /// Like the sync method, this does not return the previous value (dropping it avoids an
    /// extra read+decode per write on IO stores); call
    /// [`ConcurrentCachedAsync::async_cache_get`] yourself if you need the prior value.
    ///
    /// # Errors
    ///
    /// Returns `Self::Error` if the operation fails.
    fn async_cache_set_ref(
        &self,
        k: &K,
        v: &V,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;
}
