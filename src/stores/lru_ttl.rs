use crate::time::Duration;
use crate::time::Instant;
use std::cmp::Eq;
use std::hash::Hash;

#[cfg(feature = "async_core")]
use {super::CachedGetOrSetAsync, std::future::Future};

use crate::{CacheExpiry, CachedIter, CachedPeek, CloneCached};

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
#[doc(alias = "TimedSizedCache")]
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
///
/// This appears as the builder's `E` type parameter default. It only encodes
/// builder state; most code never names it directly.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoEvict;

/// Typestate marker for [`LruTtlCacheBuilder`]: eviction callback has been set.
///
/// When this marker is active, [`LruTtlCacheBuilder::build`] requires
/// `K: 'static` and `V: 'static` because the callback must be wired into
/// the inner LRU store. It only encodes builder state; most code never
/// names it directly.
#[derive(Clone, Copy, Debug, Default)]
pub struct HasEvict;

/// Builder for [`LruTtlCache`].
///
/// Obtain one via [`LruTtlCache::builder`].
///
/// The `S` type parameter selects the hash builder; it defaults to [`DefaultHashBuilder`].
/// Call [`.hasher()`](LruTtlCacheBuilder::hasher) to use a custom hasher. It sits in the
/// third slot, matching every other builder in the crate
/// (`LruCacheBuilder<K, V, S>`, `TtlCacheBuilder<K, V, S>`, and so on), so
/// `LruTtlCacheBuilder<K, V, MyHasher>` means what it looks like it means.
///
/// The trailing `E` type parameter is a compile-time marker:
/// - [`NoEvict`] (the default): no eviction callback has been set; `build`
///   does **not** require `K: 'static` or `V: 'static`.
/// - [`HasEvict`]: an eviction callback was registered via [`on_evict`](LruTtlCacheBuilder::on_evict);
///   `build` requires `K: 'static + V: 'static` so the callback
///   can be wired into the inner LRU eviction path.
pub struct LruTtlCacheBuilder<K, V, S = DefaultHashBuilder, E = NoEvict> {
    size: Option<usize>,
    ttl: Option<Duration>,
    refresh: bool,
    on_evict: Option<super::OnEvict<K, V>>,
    hasher: S,
    _evict: PhantomData<E>,
}

impl<K, V> Default for LruTtlCacheBuilder<K, V> {
    fn default() -> Self {
        Self {
            size: None,
            ttl: None,
            refresh: false,
            on_evict: None,
            hasher: super::new_default_hash_builder(),
            _evict: PhantomData,
        }
    }
}

impl<K, V> LruTtlCacheBuilder<K, V> {
    /// Create a builder with default settings. Equivalent to [`LruTtlCache::builder`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

// size / ttl / refresh work regardless of eviction state or hasher
impl<K, V, S, E> LruTtlCacheBuilder<K, V, S, E> {
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
    pub fn hasher<S2: BuildHasher>(self, hasher: S2) -> LruTtlCacheBuilder<K, V, S2, E> {
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
impl<K, V, S> LruTtlCacheBuilder<K, V, S, NoEvict> {
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
    /// `LruTtlCacheBuilder<K, V, S, `[`HasEvict`]`>`, which requires `K: 'static`
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
    ) -> LruTtlCacheBuilder<K, V, S, HasEvict> {
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
impl<K, V, S: BuildHasher> LruTtlCacheBuilder<K, V, S, NoEvict> {
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
impl<K, V, S: BuildHasher> LruTtlCacheBuilder<K, V, S, HasEvict> {
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

    /// Same as [`entry_live`](Self::entry_live) but takes an already-sampled `now`
    /// instead of reading the clock. Lets hot paths that already have `now` in hand
    /// (e.g. a caller that just computed a fresh expiry, or a sweep that snapshotted
    /// the clock once for the whole pass) avoid a redundant clock read.
    ///
    /// The boundary convention is identical: an entry is live only while
    /// `now < expires_at`, so `now == expires_at` is already expired.
    #[inline]
    pub(super) fn entry_live_at(expires_at: Option<Instant>, now: Instant) -> bool {
        expires_at.is_none_or(|t| now < t)
    }

    /// Insert `entry` for `key`, returning the previous value only if it was still live.
    ///
    /// A displaced expired value is filtered from the return (matching the get paths), so it is
    /// dropped silently from the caller's view; in that case fire `on_evict` and count an
    /// eviction. The inner `LruCache::cache_set` does not fire `on_evict` on an overwrite, so the
    /// callback fires exactly once here. The key is cloned only when a callback is configured.
    ///
    /// `now` is the caller's already-sampled clock reading (the same one that produced
    /// `entry.expires_at`), used to decide whether the displaced entry was still live --
    /// avoids a second `Instant::now()` call here.
    fn set_entry(&mut self, key: K, entry: TimedEntry<V>, now: Instant) -> Option<V> {
        match self.store.cache_set_returning_entry(key, entry) {
            Some((_, old)) if Self::entry_live_at(old.expires_at, now) => Some(old.value),
            Some((stored_key, old)) => {
                // Count BEFORE notifying: a panicking callback must never leave
                // an entry removed-but-uncounted.
                self.evictions.fetch_add(1, Ordering::Relaxed);
                if let Some(on_evict) = &self.on_evict {
                    on_evict(&stored_key, &old.value);
                }
                None
            }
            None => None,
        }
    }

    /// Compute the expiry instant for a new or refreshed entry given the current TTL.
    /// Returns `None` when `ttl` is zero (expiry disabled), or `Some(now + ttl)`.
    /// On overflow (`now + ttl` exceeds `Instant`'s representable range, a TTL on the
    /// order of hundreds of years) returns `None`: the entry never expires.
    ///
    /// This does NOT match the sharded TTL stores, which clamp the configured ttl to
    /// `u64::MAX` nanoseconds (~584 years) before computing a deadline, so their
    /// `checked_add` is practically unreachable and they stamp a real far-future
    /// `Instant` instead of `None`. See `specs/design/0048-ttl-overflow-vs-clamp.md`;
    /// `extreme_ttl_diverges_between_single_owner_and_sharded_ttl_families` in
    /// `tests/v3_per_key_expiry_read.rs` pins both sides.
    #[inline]
    pub(super) fn compute_expires_at(ttl: Duration, now: Instant) -> Option<Instant> {
        if ttl.is_zero() {
            None
        } else {
            now.checked_add(ttl)
        }
    }

    /// Expiry instant for an entry whose TTL is being refreshed on hit.
    ///
    /// A zero TTL means expiry is disabled, and disabling expiry must not silently clear a
    /// deadline an entry already carries, so `current` is kept unchanged. Otherwise the
    /// deadline is recomputed from `now` and -- exactly like a fresh insert through
    /// [`compute_expires_at`](Self::compute_expires_at) -- an overflowing `now + ttl` yields
    /// `None`, i.e. never expires. Writing `compute_expires_at(ttl, now).or(current)` instead
    /// would conflate the two `None` cases and leave a refreshed entry pinned to its old, much
    /// shorter deadline under a TTL so large it overflows `Instant`.
    #[inline]
    pub(super) fn refreshed_expires_at(
        ttl: Duration,
        now: Instant,
        current: Option<Instant>,
    ) -> Option<Instant> {
        if ttl.is_zero() {
            current
        } else {
            now.checked_add(ttl)
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

    /// Return all live entries in the current order from most to least recently
    /// used, as `(K, `[`CacheValue`](super::CacheValue)`)` pairs. The wrapper
    /// `Deref`s to `V` and exposes the entry's expiry via
    /// [`expires_at`](super::CacheValue::expires_at).
    /// Items past their expiry will be excluded.
    #[must_use]
    pub fn iter_order(&self) -> Vec<(K, super::CacheValue<V, Option<Instant>>)>
    where
        K: Clone,
        V: Clone,
    {
        // One clock reading for the whole eager pass (as in `evict`), so liveness is
        // judged against a single consistent instant instead of a per-entry read.
        let now = Instant::now();
        // `LRUListIterator` has no `size_hint`, so `collect` would grow the Vec from
        // zero; the stored entry count is a known upper bound on the live entries.
        let mut out = Vec::with_capacity(self.store.cache_size());
        out.extend(self.store.order.iter().filter_map(|(k, entry)| {
            let expires_at = entry.expires_at;
            if Self::entry_live_at(expires_at, now) {
                Some((
                    k.clone(),
                    super::CacheValue::new(entry.value.clone(), expires_at),
                ))
            } else {
                None
            }
        }));
        out
    }

    /// Return a `Vec` of keys in the current order from most
    /// to least recently used.
    /// Items past their expiry will be excluded.
    #[must_use]
    pub fn key_order(&self) -> Vec<K>
    where
        K: Clone,
    {
        // Single clock reading + pre-sized output, as in `iter_order`.
        let now = Instant::now();
        let mut out = Vec::with_capacity(self.store.cache_size());
        out.extend(self.store.order.iter().filter_map(|(k, entry)| {
            if Self::entry_live_at(entry.expires_at, now) {
                Some(k.clone())
            } else {
                None
            }
        }));
        out
    }

    /// Return a `Vec` of [`CacheValue`](super::CacheValue)-wrapped values (each
    /// carrying its expiry) in the current order from most to least recently used.
    /// Items past their expiry will be excluded.
    #[must_use]
    pub fn value_order(&self) -> Vec<super::CacheValue<V, Option<Instant>>>
    where
        V: Clone,
    {
        // Single clock reading + pre-sized output, as in `iter_order`.
        let now = Instant::now();
        let mut out = Vec::with_capacity(self.store.cache_size());
        out.extend(self.store.order.iter().filter_map(|(_k, entry)| {
            let expires_at = entry.expires_at;
            if Self::entry_live_at(expires_at, now) {
                Some(super::CacheValue::new(entry.value.clone(), expires_at))
            } else {
                None
            }
        }));
        out
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

    /// Evict expired values from the cache.
    #[must_use]
    pub fn evict(&mut self) -> usize {
        let now = Instant::now();
        // Two-phase: select, then remove, then count, then notify. The scan collects every
        // doomed slot before unlinking any of them, so counting or notifying from inside the
        // scan predicate would fire the side effects
        // for entries that are still stored -- and a panic anywhere in the scan would leave
        // them served after their `on_evict` cleanup already ran.
        // None means never-expires; Some(t) expires when now >= t.
        let doomed = self.doomed_indices(|_key, entry| !Self::entry_live_at(entry.expires_at, now));
        self.remove_and_notify(doomed)
    }

    /// Phase 1 of a two-phase sweep: inner-store slot indices (MRU -> LRU) of the entries
    /// `doomed` selects.
    ///
    /// Reads only, so a panic out of `doomed` (it runs the caller's `retain` predicate)
    /// leaves the cache exactly as it was: nothing removed, nothing counted, nothing
    /// notified. Mirrors `LruCache::retain`'s scan.
    fn doomed_indices<F: FnMut(&K, &TimedEntry<V>) -> bool>(&self, mut doomed: F) -> Vec<usize> {
        let mut out = Vec::with_capacity(self.store.store.len());
        out.extend(self.store.order.iter_indices().filter(|&index| {
            let (key, entry) = self.store.order.get(index);
            doomed(key, entry)
        }));
        out
    }

    /// Phase 2 of a two-phase sweep: remove every selected slot, then count the batch as
    /// evictions, then notify `on_evict`. Returns the number of entries removed.
    ///
    /// Indices stay valid because nothing is inserted between the scan and the removals.
    /// Every entry is out of the store and counted before the first notification, so a
    /// panicking `on_evict` can never leave a cleaned-up entry still reachable, nor an
    /// entry removed-but-uncounted.
    fn remove_and_notify(&mut self, doomed: Vec<usize>) -> usize {
        let removed: Vec<(K, TimedEntry<V>)> = doomed
            .into_iter()
            .map(|index| self.store.remove_index(index))
            .collect();
        if !removed.is_empty() {
            self.evictions
                .fetch_add(removed.len() as u64, Ordering::Relaxed);
        }
        if let Some(on_evict) = &self.on_evict {
            for (key, entry) in &removed {
                on_evict(key, &entry.value);
            }
        }
        removed.len()
    }

    /// Retain only entries that are unexpired and satisfy `keep`.
    ///
    /// Iterates the entries held in the underlying LRU store (most- to
    /// least-recently-used) and removes every entry that is already TTL-expired
    /// **or** for which `keep` returns `false` — expired entries are removed
    /// without consulting `keep`. `on_evict` is called and the eviction counter
    /// incremented for each removed entry. The LRU recency order of the
    /// surviving entries is unchanged.
    ///
    /// This matches [`ExpiringLruCache::retain`](crate::ExpiringLruCache::retain); the plain
    /// [`LruCache::retain`](crate::LruCache::retain) has no expiry dimension and
    /// removes solely on the predicate.
    ///
    /// Returns the number of entries removed: the count folds together entries `keep`
    /// rejected and entries swept for having already expired, since expiry removal is
    /// unconditional regardless of what `keep` returns. `retain` is deliberately not
    /// `#[must_use]`: discarding the count is a legitimate and common use, matching
    /// existing bare `cache.retain(...);` call sites.
    pub fn retain<F: FnMut(&K, &V) -> bool>(&mut self, mut keep: F) -> usize {
        // One clock reading for the whole pass (as in `evict`): every entry is judged
        // against the same instant instead of re-reading the clock per entry.
        let now = Instant::now();
        // Two-phase (see `doomed_indices` / `remove_and_notify`): the selection pass must be
        // side-effect free so a panicking `keep` leaves the cache untouched rather than
        // half-notified with every scanned entry still stored.
        let doomed = self.doomed_indices(|key, entry| {
            let expired = !Self::entry_live_at(entry.expires_at, now);
            expired || !keep(key, &entry.value)
        });
        self.remove_and_notify(doomed)
    }

    /// Remove all entries and fire the `on_evict` callback for each one, incrementing the
    /// evictions counter.
    ///
    /// Unlike [`cache_clear`](crate::Cached::cache_clear) (which removes entries silently),
    /// this method invokes `on_evict` for every removed entry (whether or not they had expired)
    /// and increments `evictions`. The eviction count does not depend on whether an
    /// `on_evict` callback is configured.
    pub fn cache_clear_with_on_evict(&mut self) {
        // `drain_all` walks the LRU chain once taking owned pairs (MRU -> LRU, the same
        // order the old `key_order` + per-key `pop_raw` drain fired in) -- no key clones
        // and no re-hashing.
        let removed = self.store.drain_all();
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
    type Error = std::convert::Infallible;

    fn cache_get<Q>(&mut self, key: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        let hash = self.store.hash(key);
        if let Some(index) = self.store.get_index(hash, key) {
            // Sample the clock ONCE for this hit and reuse it for both the liveness
            // check and the `refresh_on_hit` expiry. Sampled after the probe so an
            // absent-key miss reads the clock not at all.
            let now = Instant::now();
            let entry = &self.store.order.get(index).1;
            if Self::entry_live_at(entry.expires_at, now) {
                self.store.order.move_to_front(index);
                self.hits.fetch_add(1, Ordering::Relaxed);
                if self.refresh {
                    let new_exp = Self::refreshed_expires_at(
                        self.ttl,
                        now,
                        self.store.order.get(index).1.expires_at,
                    );
                    self.store.order.get_mut(index).1.expires_at = new_exp;
                }
                Some(&self.store.order.get(index).1.value)
            } else {
                self.misses.fetch_add(1, Ordering::Relaxed);
                // The key's hash is already in hand from the probe above; the lazy
                // sweep must not recompute it (this is the steady-state path -- every
                // entry takes it exactly once).
                if let Some((k, entry)) = self.store.pop_raw_with_hash(hash, key) {
                    // Count BEFORE notifying: a panicking callback must never leave
                    // an entry removed-but-uncounted.
                    self.evictions.fetch_add(1, Ordering::Relaxed);
                    if let Some(on_evict) = &self.on_evict {
                        on_evict(&k, &entry.value);
                    }
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
            // One clock reading per hit, reused by the refresh below (as in `cache_get`).
            let now = Instant::now();
            let entry = &self.store.order.get(index).1;
            if Self::entry_live_at(entry.expires_at, now) {
                self.store.order.move_to_front(index);
                self.hits.fetch_add(1, Ordering::Relaxed);
                if self.refresh {
                    let new_exp = Self::refreshed_expires_at(
                        self.ttl,
                        now,
                        self.store.order.get(index).1.expires_at,
                    );
                    self.store.order.get_mut(index).1.expires_at = new_exp;
                }
                Some(&mut self.store.order.get_mut(index).1.value)
            } else {
                self.misses.fetch_add(1, Ordering::Relaxed);
                // Reuse the probe's hash for the lazy sweep (as in `cache_get`).
                if let Some((k, entry)) = self.store.pop_raw_with_hash(hash, key) {
                    // Count BEFORE notifying: a panicking callback must never leave
                    // an entry removed-but-uncounted.
                    self.evictions.fetch_add(1, Ordering::Relaxed);
                    if let Some(on_evict) = &self.on_evict {
                        on_evict(&k, &entry.value);
                    }
                }
                None
            }
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
            None
        }
    }

    fn cache_get_or_set_with_mut<F: FnOnce() -> V>(&mut self, key: K, f: F) -> &mut V {
        let ttl = self.ttl;
        // Count the miss the instant the setter runs, mirroring the try-path and `TtlCache`:
        // the inner store calls the setter only when the lookup found no live entry (absent
        // key or expired entry), so a hit never counts one, and a panicking `f` still records
        // the miss before unwinding through `get_or_set_with_if` (EXP-2).
        // Count the miss the instant the setter runs, mirroring the try-path and `TtlCache`:
        // the inner store calls the setter only when the lookup found no live entry (absent
        // key or expired entry), so a hit never counts one, and a panicking `f` still records
        // the miss before unwinding through `get_or_set_with_if` (EXP-2).
        let misses = &self.misses;
        let setter = move || {
            misses.fetch_add(1, Ordering::Relaxed);
            // Anchor the expiry AFTER the factory runs so a slow factory does
            // not eat into the fresh entry's TTL (CORE-3). This clock read is
            // deliberately NOT shared with `hit_at` below: `f()` may run arbitrarily
            // long, so the fresh expiry must be anchored once it returns.
            let value = f();
            let now = Instant::now();
            let expires_at = Self::compute_expires_at(ttl, now);
            TimedEntry { expires_at, value }
        };
        // The store calls the validity closure only when the key is present, so sample
        // the clock there and reuse the reading for the refresh below: one read per hit,
        // and the insert path (which anchors its own expiry after the factory) pays none.
        let mut hit_at: Option<Instant> = None;
        // On replacement the store returns the STORED key/entry of the displaced value, so the
        // callback sees the instance that was actually cached, not the (equal-but-distinct)
        // lookup key (C1/C8).
        let (was_present, was_valid, old_entry, entry) =
            self.store.get_or_set_with_if(key, setter, |entry| {
                Self::entry_live_at(entry.expires_at, *hit_at.insert(Instant::now()))
            });
        if was_present && was_valid {
            if self.refresh {
                let now = hit_at.unwrap_or_else(Instant::now);
                let new_exp = Self::refreshed_expires_at(self.ttl, now, entry.expires_at);
                entry.expires_at = new_exp;
            }
            self.hits.fetch_add(1, Ordering::Relaxed);
        } else if let Some((old_key, old)) = old_entry {
            // The miss was already counted by `setter`.
            // Count BEFORE notifying: a panicking callback must never leave
            // an entry removed-but-uncounted.
            self.evictions.fetch_add(1, Ordering::Relaxed);
            if let Some(on_evict) = &self.on_evict {
                on_evict(&old_key, &old.value);
            }
        }
        &mut entry.value
    }

    fn cache_try_get_or_set_with_mut<F: FnOnce() -> Result<V, E>, E>(
        &mut self,
        key: K,
        f: F,
    ) -> Result<&mut V, E> {
        let ttl = self.ttl;
        // Count the miss the instant the setter runs. The inner store calls the setter only
        // when the lookup found no live entry (absent key or expired entry), so a hit never
        // counts one; and because the increment lands before `f` returns, an `Err` factory
        // still records the miss instead of losing it on the `?` early return below. This
        // matches `TtlCache` and `ExpiringLruCache`'s try-path accounting (EXP-2).
        let misses = &self.misses;
        let setter = move || {
            misses.fetch_add(1, Ordering::Relaxed);
            // Anchor the expiry after the factory succeeds (CORE-3); deliberately a
            // fresh clock read, not the `hit_at` sample taken before `f()` ran.
            let value = f()?;
            let now = Instant::now();
            let expires_at = Self::compute_expires_at(ttl, now);
            Ok(TimedEntry { expires_at, value })
        };
        // One clock read per hit, shared by the liveness check and the refresh below.
        let mut hit_at: Option<Instant> = None;
        // On replacement the store returns the STORED key/entry of the displaced value (C1/C8).
        let (was_present, was_valid, old_entry, entry) =
            self.store.try_get_or_set_with_if(key, setter, |entry| {
                Self::entry_live_at(entry.expires_at, *hit_at.insert(Instant::now()))
            })?;
        if was_present && was_valid {
            if self.refresh {
                let now = hit_at.unwrap_or_else(Instant::now);
                let new_exp = Self::refreshed_expires_at(self.ttl, now, entry.expires_at);
                entry.expires_at = new_exp;
            }
            self.hits.fetch_add(1, Ordering::Relaxed);
        } else if let Some((old_key, old)) = old_entry {
            // The miss was already counted by `setter`. On `Err` the expired entry is left
            // in place, so `on_evict` / `evictions` deliberately stay behind until a call
            // actually displaces it -- firing early would double-fire for one physical entry.
            // Count BEFORE notifying: a panicking callback must never leave an entry
            // removed-but-uncounted.
            self.evictions.fetch_add(1, Ordering::Relaxed);
            if let Some(on_evict) = &self.on_evict {
                on_evict(&old_key, &old.value);
            }
        }
        Ok(&mut entry.value)
    }

    /// Insert a key-value pair. Returns the previous value only if it had not yet expired.
    /// An expired previous value is filtered from the return; it fires `on_evict` and counts as
    /// an eviction, matching the other removal paths.
    ///
    /// Overwriting an existing key promotes it to most-recently-used: a write counts as an
    /// access, so the entry moves to the front of the eviction order exactly as a fresh
    /// insertion would (its expiry is also reset from the current TTL). Use
    /// [`CachedPeek::cache_peek`](crate::CachedPeek::cache_peek) if you need to inspect an
    /// entry without touching recency.
    fn cache_set(&mut self, key: K, val: V) -> Option<V> {
        let now = Instant::now();
        let expires_at = Self::compute_expires_at(self.ttl, now);
        // `now` is threaded through: `set_entry` judges the displaced entry's liveness
        // against this same reading instead of sampling the clock a second time.
        self.set_entry(
            key,
            TimedEntry {
                expires_at,
                value: val,
            },
            now,
        )
    }

    fn cache_remove<Q>(&mut self, k: &Q) -> Option<V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        if let Some((stored_k, entry)) = self.store.pop_raw(k) {
            // Judge liveness at the moment of removal, BEFORE the callback runs. Sampling
            // it afterwards would let a slow `on_evict` push the entry past its deadline and
            // report `None` for a value that was live when it was taken out.
            let live = Self::entry_live(entry.expires_at);
            // Count BEFORE notifying: a panicking callback must never leave an
            // entry removed-but-uncounted.
            self.evictions.fetch_add(1, Ordering::Relaxed);
            if let Some(on_evict) = &self.on_evict {
                on_evict(&stored_k, &entry.value);
            }
            if live { Some(entry.value) } else { None }
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
            // Count BEFORE notifying: a panicking callback must never leave an
            // entry removed-but-uncounted.
            self.evictions.fetch_add(1, Ordering::Relaxed);
            if let Some(on_evict) = &self.on_evict {
                on_evict(&stored_k, &entry.value);
            }
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
        // Delegate to the inner LruCache's reset which preserves the hash builder and
        // already resets the inner metrics. Reset outer-level metrics here directly to
        // avoid a redundant second call to the inner store's cache_reset_metrics.
        self.store.cache_reset();
        self.hits.store(0, Ordering::Relaxed);
        self.misses.store(0, Ordering::Relaxed);
        self.evictions.store(0, Ordering::Relaxed);
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

    /// Check whether the cache contains a live (non-expired) entry for `k`.
    ///
    /// Delegates to [`CachedPeek::cache_peek`], so it records no hit/miss
    /// metrics, performs no recency promotion or TTL refresh, and reports
    /// absent/expired entries as `false`.
    fn cache_contains<Q>(&mut self, k: &Q) -> bool
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        crate::CachedPeek::cache_peek(self, k).is_some()
    }
}

impl<K: Hash + Eq + Clone, V, S: BuildHasher> CachedIter<K, V> for LruTtlCache<K, V, S> {
    fn iter<'a>(&'a self) -> impl Iterator<Item = (&'a K, &'a V)> + 'a
    where
        K: 'a,
        V: 'a,
    {
        // Deliberately per-item `entry_live` (a fresh clock read each step) rather than
        // a snapshot taken when the iterator is built: this iterator is lazy and may be
        // held across arbitrary time, so a construction-time `now` would be observable
        // -- entries that expired mid-iteration would still be yielded. The eager
        // collectors (`iter_order`/`key_order`/`value_order`) hoist their reading
        // because they complete in one pass.
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
}

impl<K: Hash + Eq + Clone, V, S: BuildHasher> crate::CacheRefreshOnHit for LruTtlCache<K, V, S> {
    fn refresh_on_hit(&self) -> bool {
        self.refresh
    }
    fn set_refresh_on_hit(&mut self, refresh: bool) -> bool {
        let old = self.refresh;
        self.refresh = refresh;
        old
    }
}

impl<K: Hash + Eq + Clone, V, S: BuildHasher> crate::CacheSetMaxSize for LruTtlCache<K, V, S> {
    fn set_max_size(&mut self, max_size: usize) -> Option<usize> {
        LruTtlCache::set_max_size(self, max_size)
    }

    fn try_set_max_size(
        &mut self,
        max_size: usize,
    ) -> Result<Option<usize>, super::SetMaxSizeError> {
        LruTtlCache::try_set_max_size(self, max_size)
    }
}

impl<K: Hash + Eq + Clone, V, S: BuildHasher> crate::CacheClearWithOnEvict
    for LruTtlCache<K, V, S>
{
    fn cache_clear_with_on_evict(&mut self) {
        LruTtlCache::cache_clear_with_on_evict(self);
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
            // One clock reading per hit, reused by the refresh below (as in `cache_get`).
            let now = Instant::now();
            let entry = &self.store.order.get(index).1;
            let expired = !Self::entry_live_at(entry.expires_at, now);
            if expired {
                self.misses.fetch_add(1, Ordering::Relaxed);
                (Some(self.store.order.get(index).1.value.clone()), true)
            } else {
                self.store.order.move_to_front(index);
                self.hits.fetch_add(1, Ordering::Relaxed);
                if self.refresh {
                    let new_exp = Self::refreshed_expires_at(
                        self.ttl,
                        now,
                        self.store.order.get(index).1.expires_at,
                    );
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

impl<K: Hash + Eq + Clone, V, S: BuildHasher> CacheExpiry<K, V> for LruTtlCache<K, V, S> {
    /// Returns the stored value and its expiry instant, with no read side effects.
    ///
    /// The instant is the entry's own deadline, `None` when the entry never expires (TTL was
    /// disabled at insert time). `None` also when `now + ttl` overflowed `Instant` at insert
    /// time, so no deadline could be recorded. An expired entry is returned with its past
    /// deadline and is **not** removed. Uses the same non-promoting lookup as
    /// [`cache_peek_with_expiry_status`](CloneCached::cache_peek_with_expiry_status): no
    /// hit/miss counting, no LRU promotion, no TTL renewal.
    ///
    /// The convention is `now >= t` means expired: a deadline exactly equal to the current
    /// instant counts as already past, matching the liveness check the store itself applies.
    fn cache_peek_expires_at<Q>(&self, k: &Q) -> (Option<V>, Option<Instant>)
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
        V: Clone,
    {
        // Use the inner LruCache's `cache_peek` to avoid LRU promotion.
        if let Some(entry) = self.store.cache_peek(k) {
            (Some(entry.value.clone()), entry.expires_at)
        } else {
            (None, None)
        }
    }

    /// Returns whether the key is present and its expiry instant, without the value.
    ///
    /// The value-free counterpart of
    /// [`cache_peek_expires_at`](CacheExpiry::cache_peek_expires_at): the same non-promoting
    /// lookup and the same deadline, with no clone and no `V: Clone` bound. `(false, None)` when
    /// the key is absent, `(true, None)` when the entry never expires (TTL disabled at insert
    /// time, or `now + ttl` overflowed `Instant`), `(true, Some(t))` otherwise. An expired entry
    /// reports `(true, Some(t))` with `t` in the past and is **not** removed. No hit/miss
    /// counting, no LRU promotion, no TTL renewal.
    ///
    /// The convention is `now >= t` means expired: a deadline exactly equal to the current
    /// instant counts as already past, matching the liveness check the store itself applies.
    fn cache_expires_at<Q>(&self, k: &Q) -> (bool, Option<Instant>)
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        // Use the inner LruCache's `cache_peek` to avoid LRU promotion.
        match self.store.cache_peek(k) {
            Some(entry) => (true, entry.expires_at),
            None => (false, None),
        }
    }
}

#[cfg(feature = "async_core")]
#[cfg_attr(docsrs, doc(cfg(feature = "async_core")))]
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
            let ttl = self.ttl;
            // Count the miss as soon as the setter future starts running, mirroring the
            // try-path twin below: the inner store only awaits this future when the lookup
            // found no live entry, so a hit never counts one, and a future dropped mid-poll
            // (or a panicking `f`) still records the miss before the outer future is torn
            // down (EXP-2).
            // Count the miss as soon as the setter future starts running, mirroring the
            // try-path twin below: the inner store only awaits this future when the lookup
            // found no live entry, so a hit never counts one, and a future dropped mid-poll
            // (or a panicking `f`) still records the miss before the outer future is torn
            // down (EXP-2).
            let misses = &self.misses;
            let setter = || async move {
                misses.fetch_add(1, Ordering::Relaxed);
                // Anchor the expiry after the factory resolves (CORE-3); deliberately a
                // fresh clock read, not the `hit_at` sample taken before it ran.
                let value = f().await;
                let now = Instant::now();
                let expires_at = Self::compute_expires_at(ttl, now);
                TimedEntry { expires_at, value }
            };
            // One clock read per hit, shared by the liveness check and the refresh below.
            let mut hit_at: Option<Instant> = None;
            // On replacement the store returns the STORED key/entry of the displaced value (C1/C8).
            let (was_present, was_valid, old_entry, entry) = self
                .store
                .get_or_set_with_if_async(key, setter, |entry| {
                    Self::entry_live_at(entry.expires_at, *hit_at.insert(Instant::now()))
                })
                .await;
            if was_present && was_valid {
                if self.refresh {
                    let now = hit_at.unwrap_or_else(Instant::now);
                    let new_exp = Self::refreshed_expires_at(self.ttl, now, entry.expires_at);
                    entry.expires_at = new_exp;
                }
                self.hits.fetch_add(1, Ordering::Relaxed);
            } else if let Some((old_key, old)) = old_entry {
                // The miss was already counted by `setter`.
                // Count BEFORE notifying: a panicking callback must never leave
                // an entry removed-but-uncounted.
                self.evictions.fetch_add(1, Ordering::Relaxed);
                if let Some(on_evict) = &self.on_evict {
                    on_evict(&old_key, &old.value);
                }
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
            let ttl = self.ttl;
            // Count the miss before awaiting the factory, so an `Err` still records it
            // instead of losing it on the `?` early return below (EXP-2); see the sync
            // `cache_try_get_or_set_with_mut` for the full rationale.
            let misses = &self.misses;
            let setter = move || async move {
                misses.fetch_add(1, Ordering::Relaxed);
                // Fresh clock read anchored after the factory resolves (CORE-3).
                let new_val = f().await?;
                let now = Instant::now();
                let expires_at = Self::compute_expires_at(ttl, now);
                Ok(TimedEntry {
                    expires_at,
                    value: new_val,
                })
            };
            // One clock read per hit, shared by the liveness check and the refresh below.
            let mut hit_at: Option<Instant> = None;
            // On replacement the store returns the STORED key/entry of the displaced value (C1/C8).
            let (was_present, was_valid, old_entry, entry) = self
                .store
                .try_get_or_set_with_if_async(key, setter, |entry| {
                    Self::entry_live_at(entry.expires_at, *hit_at.insert(Instant::now()))
                })
                .await?;
            if was_present && was_valid {
                if self.refresh {
                    let now = hit_at.unwrap_or_else(Instant::now);
                    let new_exp = Self::refreshed_expires_at(self.ttl, now, entry.expires_at);
                    entry.expires_at = new_exp;
                }
                self.hits.fetch_add(1, Ordering::Relaxed);
            } else if let Some((old_key, old)) = old_entry {
                // The miss was already counted by `setter`; on `Err` the expired entry is
                // still stored, so the eviction side deliberately waits for the call that
                // actually displaces it. Count BEFORE notifying: a panicking callback must
                // never leave an entry removed-but-uncounted.
                self.evictions.fetch_add(1, Ordering::Relaxed);
                if let Some(on_evict) = &self.on_evict {
                    on_evict(&old_key, &old.value);
                }
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
    fn iter_order_and_value_order_expose_expiry_via_cache_value() {
        let mut c: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        let before = Instant::now();
        c.cache_set(1, 10);
        c.cache_set(2, 20);

        // MRU-first order; the wrapper Derefs to V and compares against bare values.
        let ordered = c.iter_order();
        assert_eq!(ordered.len(), 2);
        assert_eq!(ordered[0].0, 2);
        assert_eq!(*ordered[0].1, 20);
        assert_eq!(ordered[1].1, 10);
        // Finite ttl: every entry carries a future expiry.
        for (_k, v) in &ordered {
            let exp = v.expires_at().expect("finite ttl entries carry an expiry");
            assert!(exp > before);
        }

        let vals = c.value_order();
        assert_eq!(vals, vec![20, 10]);
        assert!(vals[0].expires_at().is_some());
        assert_eq!(vals[0].value(), &20);
        assert_eq!(vals.into_iter().map(|v| v.into_value()).sum::<u32>(), 30);
    }

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
    fn cache_set_over_expired_counts_eviction_without_callback() {
        // Pins that the evictions counter increments when overwriting an expired entry
        // even when no on_evict callback is configured.
        let mut c: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_millis(20))
            .build()
            .unwrap();
        c.cache_set(1, 100);
        let before = c.cache_evictions().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(60));
        // Expired entry: overwrite filters it from the return and counts one eviction.
        assert_eq!(c.cache_set(1, 200), None);
        assert_eq!(
            c.cache_evictions(),
            Some(before + 1),
            "evictions must increment by 1 on expired-entry overwrite even without on_evict"
        );
        // Overwriting the now-live value must not count as an eviction.
        assert_eq!(c.cache_set(1, 300), Some(200));
        assert_eq!(
            c.cache_evictions(),
            Some(before + 1),
            "overwriting a live entry must not increment evictions"
        );
    }

    #[test]
    fn cache_set_with_ttl_overflow_stores_never_expiring_entry() {
        // A TTL that would overflow Instant bounds (compute_expires_at's
        // now.checked_add(ttl) -> None) stores the entry with no expiry: it never
        // expires, matching TtlSortedCache's set_with(..).ttl(..) overflow behavior.
        use crate::CacheTtl;
        let mut c: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        c.set_ttl(Duration::MAX);
        assert_eq!(c.cache_set(1, 42), None);
        assert_eq!(c.cache_get(&1), Some(&42));
        // Never-expiring: CacheValue's expires_at() metadata must be None.
        let ordered = c.iter_order();
        assert_eq!(ordered.len(), 1);
        assert_eq!(*ordered[0].1, 42);
        assert_eq!(ordered[0].1.expires_at(), None);
    }

    #[test]
    fn cache_set_over_existing_key_promotes_to_mru() {
        let mut c: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(3)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        c.cache_set(1, 10);
        c.cache_set(2, 20);
        c.cache_set(3, 30);
        assert_eq!(c.key_order(), vec![3, 2, 1]);
        // Overwriting the least-recently-used key returns the old (still-live) value
        // and promotes the entry to most-recently-used.
        assert_eq!(c.cache_set(1, 11), Some(10));
        assert_eq!(c.key_order(), vec![1, 3, 2]);
        assert_eq!(c.cache_get(&1), Some(&11));
    }

    #[test]
    fn cache_set_promotion_changes_the_capacity_eviction_victim() {
        let mut c: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(3)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        c.cache_set(1, 10);
        c.cache_set(2, 20);
        c.cache_set(3, 30);
        // 1 was the LRU victim; overwriting it makes 2 the victim instead.
        assert_eq!(c.cache_set(1, 11), Some(10));
        c.cache_set(4, 40);
        assert_eq!(c.key_order(), vec![4, 1, 3]);
        assert_eq!(c.cache_get(&2), None, "2 became the LRU victim");
        assert_eq!(c.cache_get(&1), Some(&11));
    }

    #[test]
    fn cache_set_over_current_mru_and_sole_entry_keep_the_list_intact() {
        let mut c: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(3)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        c.cache_set(1, 10);
        c.cache_set(2, 20);
        c.cache_set(3, 30);
        // Overwriting the head must not corrupt the chain.
        assert_eq!(c.cache_set(3, 33), Some(30));
        assert_eq!(c.key_order(), vec![3, 2, 1]);
        assert_eq!(c.value_order(), vec![33, 20, 10]);
        assert_eq!(c.cache_size(), 3);

        // Sole entry of a 1-capacity cache.
        let mut d: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(1)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        d.cache_set(1, 10);
        assert_eq!(d.cache_set(1, 11), Some(10));
        assert_eq!(d.key_order(), vec![1]);
        assert_eq!(d.cache_size(), 1);
    }

    #[test]
    fn cache_peek_still_does_not_promote_after_set_does() {
        use crate::CachedPeek;
        let mut c: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(3)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        c.cache_set(1, 10);
        c.cache_set(2, 20);
        c.cache_set(3, 30);
        assert_eq!(c.cache_peek(&1), Some(&10));
        assert_eq!(c.key_order(), vec![3, 2, 1], "peek must not promote");
        assert_eq!(c.cache_set(1, 11), Some(10));
        assert_eq!(c.key_order(), vec![1, 3, 2]);
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
    fn cache_reset_zeroes_all_metrics() {
        // CLN-2: cache_reset must reset metrics exactly once; verify the result is zero,
        // including the inner LruCache's own capacity-eviction counter.
        let mut c: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(2)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        c.cache_set(1, 10);
        c.cache_set(2, 20);
        // Drive an inner-store capacity eviction so the inner evictions counter is non-zero
        // before the reset. A half-reset that only touched the outer counter would leave this.
        c.cache_set(3, 30); // evicts LRU (1) in the inner LruCache
        assert!(
            c.store.cache_evictions().unwrap() >= 1,
            "precondition: inner store must record a capacity eviction before reset"
        );
        // Drive hits and misses too.
        let _ = c.cache_get(&2);
        let _ = c.cache_get(&99);
        c.cache_reset();
        assert_eq!(
            c.cache_hits(),
            Some(0),
            "hits must be zero after cache_reset"
        );
        assert_eq!(
            c.cache_misses(),
            Some(0),
            "misses must be zero after cache_reset"
        );
        assert_eq!(
            c.cache_evictions(),
            Some(0),
            "evictions must be zero after cache_reset"
        );
        assert_eq!(
            c.store.cache_evictions(),
            Some(0),
            "inner store evictions must be zero after cache_reset"
        );
        assert_eq!(c.cache_size(), 0, "size must be zero after cache_reset");
    }

    #[test]
    fn cache_reset_metrics_standalone_zeroes_outer_and_inner() {
        // CLN-2 (regression guard): cache_reset_metrics() called on its own — NOT via
        // cache_reset — must zero BOTH the outer counters (hits/misses/evictions) AND the
        // inner LruCache's counters, while leaving stored entries untouched. Unlike
        // cache_reset (which rebuilds the inner store and thus trivially clears its metrics),
        // cache_reset_metrics must explicitly delegate to store.cache_reset_metrics(). If the
        // CLN-2 restructure had left this method only touching the outer counter, the inner
        // capacity-eviction count would survive and this test fails.
        let mut c: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(2)
            .ttl(Duration::from_millis(20))
            .build()
            .unwrap();
        // Inner capacity eviction: 3 inserts into a size-2 cache evicts the LRU key.
        c.cache_set(1, 10);
        c.cache_set(2, 20);
        c.cache_set(3, 30); // inner evictions -> 1
        assert!(
            c.store.cache_evictions().unwrap() >= 1,
            "precondition: inner store must record a capacity eviction"
        );
        // Outer metrics: a hit, a miss, and an expiry eviction.
        let _ = c.cache_get(&3); // live -> hit
        let _ = c.cache_get(&99); // miss
        std::thread::sleep(std::time::Duration::from_millis(40));
        c.cache_set(3, 40); // overwrite now-expired key 3 -> outer eviction, returns None
        assert!(
            c.hits.load(Ordering::Relaxed) >= 1,
            "precondition: outer hits must be non-zero"
        );
        assert!(
            c.evictions.load(Ordering::Relaxed) >= 1,
            "precondition: outer evictions must be non-zero"
        );

        c.cache_reset_metrics();

        assert_eq!(
            c.hits.load(Ordering::Relaxed),
            0,
            "outer hits must be zero after standalone cache_reset_metrics"
        );
        assert_eq!(
            c.misses.load(Ordering::Relaxed),
            0,
            "outer misses must be zero after standalone cache_reset_metrics"
        );
        assert_eq!(
            c.evictions.load(Ordering::Relaxed),
            0,
            "outer evictions must be zero after standalone cache_reset_metrics"
        );
        assert_eq!(
            c.store.cache_evictions(),
            Some(0),
            "inner store evictions must be zero after standalone cache_reset_metrics"
        );
        assert_eq!(
            c.cache_evictions(),
            Some(0),
            "combined (outer + inner) evictions must be zero after cache_reset_metrics"
        );
        // cache_reset_metrics must NOT drop stored entries.
        assert!(
            c.cache_size() >= 1,
            "cache_reset_metrics must not clear stored entries"
        );
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
            Err(super::super::SetMaxSizeError::ZeroMaxSize)
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
    fn cache_remove_entry_with_panicking_on_evict_still_counts_eviction() {
        // The entry is popped and counted BEFORE `on_evict` runs, so a panicking
        // callback must not leave the removed entry uncounted.
        use std::panic::{AssertUnwindSafe, catch_unwind};
        let mut c = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_secs(60))
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
    fn retain_with_panicking_on_evict_still_counts_eviction() {
        // `retain` removes every selected entry and counts the batch BEFORE the first
        // notification, so a panicking callback still leaves the eviction counted (and
        // the entry gone).
        use std::panic::{AssertUnwindSafe, catch_unwind};
        let mut c = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_secs(60))
            .on_evict(|_k: &u32, _v: &u32| panic!("boom"))
            .build()
            .unwrap();
        c.cache_set(1u32, 10u32);
        let r = catch_unwind(AssertUnwindSafe(|| c.retain(|_, _| false)));
        assert!(r.is_err(), "on_evict should have panicked");
        assert_eq!(
            c.cache_evictions(),
            Some(1),
            "eviction must be counted even though on_evict panicked"
        );
    }

    #[test]
    fn cache_get_lazy_sweep_with_panicking_on_evict_still_counts_eviction() {
        // `cache_get`'s lazy-sweep path pops the expired entry and counts the
        // eviction BEFORE `on_evict` runs, so a panicking callback must not leave
        // the swept entry uncounted.
        use std::panic::{AssertUnwindSafe, catch_unwind};
        let mut c = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_millis(20))
            .on_evict(|_k: &u32, _v: &u32| panic!("boom"))
            .build()
            .unwrap();
        c.cache_set(1u32, 10u32);
        std::thread::sleep(std::time::Duration::from_millis(80));
        let r = catch_unwind(AssertUnwindSafe(|| {
            let _ = c.cache_get(&1u32);
        }));
        assert!(r.is_err(), "on_evict should have panicked");
        assert_eq!(
            c.cache_evictions(),
            Some(1),
            "eviction must be counted even though on_evict panicked"
        );
    }

    #[test]
    fn cache_set_over_expired_with_panicking_on_evict_still_counts_eviction() {
        // Overwriting an already-expired entry fires `on_evict` for the displaced
        // value; `set_entry` counts the eviction BEFORE notifying.
        use std::panic::{AssertUnwindSafe, catch_unwind};
        let mut c = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_millis(20))
            .on_evict(|_k: &u32, _v: &u32| panic!("boom"))
            .build()
            .unwrap();
        c.cache_set(1u32, 10u32);
        std::thread::sleep(std::time::Duration::from_millis(80));
        let r = catch_unwind(AssertUnwindSafe(|| c.cache_set(1u32, 20u32)));
        assert!(r.is_err(), "on_evict should have panicked");
        assert_eq!(
            c.cache_evictions(),
            Some(1),
            "eviction must be counted even though on_evict panicked"
        );
    }

    #[test]
    fn cache_get_or_set_with_mut_panic_then_retry_counts_miss_before_factory_runs() {
        // Regression: the infallible get-or-set path must count the miss BEFORE
        // invoking the factory, matching `TtlCache` and `LruTtlCache`'s own
        // `try_*` paths. A panicking factory unwinds through the inner store
        // call, so a miss counted only after that call returns is lost; a miss
        // counted before the factory runs survives the unwind.
        use std::panic::{AssertUnwindSafe, catch_unwind};
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
        c.cache_set(1u32, 100u32);
        std::thread::sleep(std::time::Duration::from_millis(80));

        // First call: the factory panics. Note: a caught panic prints to
        // stderr; that is expected.
        let r = catch_unwind(AssertUnwindSafe(|| {
            let _ = c.cache_get_or_set_with_mut(1u32, || -> u32 { panic!("factory panic") });
        }));
        assert!(r.is_err(), "expected panic to be caught");

        // Safety invariants that must hold regardless of the miss-timing fix:
        // the expired entry is left in place, on_evict does not fire, and no
        // eviction is counted for a factory that never returned.
        assert_eq!(
            fired.load(AtomicOrdering::Relaxed),
            0,
            "on_evict must not fire when factory panics"
        );
        assert_eq!(
            c.cache_evictions(),
            Some(0),
            "evictions must remain 0 when factory panics"
        );
        assert_eq!(c.cache_size(), 1, "expired entry must still be present");
        // `cache_peek` filters expired entries, so use the status-reporting
        // peek to confirm the stale VALUE itself (not just the count) survived
        // the panic untouched.
        assert_eq!(
            c.cache_peek_with_expiry_status(&1u32),
            (Some(100), true),
            "the stale entry itself must be undisturbed by the panic"
        );

        // The miss must be counted even though the factory never returned --
        // this is the assertion that fails against the pre-fix code, which
        // only bumped `misses` in the unreachable code after the panicking
        // call.
        assert_eq!(
            c.cache_misses(),
            Some(1),
            "a panicking factory must still count a miss, matching TtlCache"
        );

        // Retry with a successful factory: this evicts the stale entry exactly
        // once and records a second miss, so the total miss count for a
        // panic-then-retry sequence matches TtlCache's behavior for the same
        // sequence (2 misses).
        let v = c.cache_get_or_set_with_mut(1u32, || 200u32);
        assert_eq!(*v, 200);
        assert_eq!(
            fired.load(AtomicOrdering::Relaxed),
            1,
            "on_evict must fire exactly once after the successful replacement"
        );
        assert_eq!(
            c.cache_evictions(),
            Some(1),
            "evictions must be 1 after the successful replacement"
        );
        assert_eq!(
            c.cache_misses(),
            Some(2),
            "panic-then-retry must total 2 misses, matching TtlCache"
        );
        assert_eq!(c.cache_hits(), Some(0), "neither call was a hit");
    }

    #[cfg(feature = "async")]
    #[tokio::test]
    async fn async_cache_get_or_set_with_mut_panic_then_retry_counts_miss_before_factory_runs() {
        // Async twin of the sync panic-then-retry regression above: the setter
        // future's miss counter must run before polling the factory future, so
        // a factory that panics mid-poll still counts a miss instead of losing
        // it when the panic unwinds out of `async_cache_get_or_set_with_mut`.
        use crate::CachedGetOrSetAsync;
        use std::panic::{AssertUnwindSafe, catch_unwind};

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
        c.cache_set(1u32, 100u32);
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;

        {
            let mut fut = Box::pin(CachedGetOrSetAsync::async_cache_get_or_set_with_mut(
                &mut c,
                1u32,
                || async { panic!("factory panic") },
            ));
            let waker = std::task::Waker::noop();
            let mut cx = std::task::Context::from_waker(waker);
            // The factory panics synchronously on its first poll (no `.await`
            // point before the `panic!`), so the panic unwinds out of this
            // `poll` call. Note: a caught panic prints to stderr; that is
            // expected.
            let r = catch_unwind(AssertUnwindSafe(|| fut.as_mut().poll(&mut cx)));
            assert!(r.is_err(), "expected panic to be caught");
            // Drop `fut` before touching `c` again -- releases the borrow.
        }

        assert_eq!(
            fired.load(AtomicOrdering::Relaxed),
            0,
            "on_evict must not fire when the factory future panics"
        );
        assert_eq!(
            c.cache_evictions(),
            Some(0),
            "evictions must remain 0 when the factory future panics"
        );
        assert_eq!(c.cache_size(), 1, "expired entry must still be present");
        assert_eq!(
            c.cache_misses(),
            Some(1),
            "a factory that panics mid-poll must still count a miss, matching the sync path"
        );

        let v =
            CachedGetOrSetAsync::async_cache_get_or_set_with_mut(&mut c, 1u32, || async { 200u32 })
                .await;
        assert_eq!(*v, 200);
        assert_eq!(
            fired.load(AtomicOrdering::Relaxed),
            1,
            "on_evict must fire exactly once after the successful replacement"
        );
        assert_eq!(
            c.cache_evictions(),
            Some(1),
            "evictions must be 1 after the successful replacement"
        );
        assert_eq!(
            c.cache_misses(),
            Some(2),
            "panic-then-retry must total 2 misses on the async path too"
        );
        assert_eq!(c.cache_hits(), Some(0), "neither call was a hit");
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

    // B1 regression: on_evict must receive the STORED key, not the caller's lookup key.
    // Key types can have fields not covered by Eq/Hash; the stored key and the new key may
    // differ in those extra fields even though they compare equal.
    #[test]
    fn on_evict_receives_stored_key_not_callers_key() {
        use std::sync::{Arc, Mutex};

        // A key whose Hash/PartialEq use only `id`; `tag` is transparent to equality.
        #[derive(Clone, Debug)]
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
        impl std::hash::Hash for TaggedKey {
            fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
                self.id.hash(state);
            }
        }

        let evicted_tags: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
        let evicted_tags2 = evicted_tags.clone();

        let mut cache: LruTtlCache<TaggedKey, u32> = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_millis(20))
            .on_evict(move |k: &TaggedKey, _v: &u32| {
                evicted_tags2.lock().unwrap().push(k.tag);
            })
            .build()
            .unwrap();

        // Insert with tag "a".
        cache.cache_set(TaggedKey { id: 1, tag: "a" }, 100);
        // Let it expire.
        std::thread::sleep(std::time::Duration::from_millis(60));
        // Overwrite with an equal key (same id) but different tag "b".
        // The displaced entry was stored with tag "a"; on_evict must report "a".
        cache.cache_set(TaggedKey { id: 1, tag: "b" }, 200);

        let tags = evicted_tags.lock().unwrap();
        assert_eq!(
            tags.as_slice(),
            &["a"],
            "on_evict must receive the stored key (tag='a'), not the caller's key (tag='b')"
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

    // =====================================================================
    // PERF-2: clock threading / eager-sweep snapshots / hash reuse.
    //
    // Every change under PERF-2 is internal-only, so the tests below pin the
    // *observable* contracts that the rewrites must not disturb:
    //   * the `now >= expires_at` expiry boundary at every converted call site,
    //   * `refresh_on_hit` extending by the FULL ttl measured from the hit,
    //   * a slow factory's expiry still anchored AFTER the factory returns,
    //   * `retain`/`evict` judging every entry against ONE pass-start snapshot,
    //   * `cache_clear_with_on_evict` firing MRU -> LRU,
    //   * the lazy expiry sweep (now reusing the probe's hash) still removing
    //     the entry it swept.
    // =====================================================================

    /// Insert an entry with an explicitly chosen `expires_at`, bypassing the ttl
    /// arithmetic, so a test can pin the exact `now >= expires_at` boundary.
    /// Writes straight into the inner `LruCache`; the outer counters are untouched.
    fn put_raw(c: &mut LruTtlCache<u32, u32>, k: u32, v: u32, expires_at: Option<Instant>) {
        c.store.cache_set(
            k,
            TimedEntry {
                expires_at,
                value: v,
            },
        );
    }

    /// Read an entry's stored `expires_at` without any read side effects
    /// (no promotion, no refresh, no metrics).
    fn stored_expiry(c: &LruTtlCache<u32, u32>, k: u32) -> Option<Instant> {
        CachedPeek::cache_peek(&c.store, &k)
            .expect("entry must be present")
            .expires_at
    }

    fn long_ttl_cache(max_size: usize) -> LruTtlCache<u32, u32> {
        LruTtlCache::builder()
            .max_size(max_size)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap()
    }

    // `entry_live_at` must preserve the exact `now >= expires_at` boundary
    // convention of `entry_live` (which reads `Instant::now()` internally):
    // at `now == expires_at` the entry is already expired.
    #[test]
    fn entry_live_at_matches_now_ge_expires_at_is_expired_convention() {
        let now = Instant::now();
        let future = now + Duration::from_millis(10);
        let past = now - Duration::from_millis(10);

        // `expires_at = None` never expires, regardless of `now`.
        assert!(LruTtlCache::<u32, u32>::entry_live_at(None, now));
        // `now < expires_at`: live.
        assert!(LruTtlCache::<u32, u32>::entry_live_at(Some(future), now));
        // `now == expires_at`: the boundary itself is NOT live.
        assert!(!LruTtlCache::<u32, u32>::entry_live_at(Some(now), now));
        // `now > expires_at`: not live.
        assert!(!LruTtlCache::<u32, u32>::entry_live_at(Some(past), now));
    }

    // --- boundary coverage at each converted call site ---------------------
    //
    // Each test crafts an entry whose `expires_at` is an `Instant` sampled just
    // before the call under test. The process clock is monotonic, so the call's
    // own internal reading is guaranteed to be `>=` that instant: this
    // deterministically exercises the "tie or later" edge without a mock clock.
    // A comfortably-future `expires_at` exercises the live side and
    // `expires_at = None` exercises "never expires".

    #[test]
    fn cache_get_boundary_matches_now_ge_expires_at_convention() {
        let mut c = long_ttl_cache(8);

        let tie = Instant::now();
        put_raw(&mut c, 1, 100, Some(tie));
        assert_eq!(
            c.cache_get(&1),
            None,
            "tie (now >= expires_at) must be a miss"
        );
        assert_eq!(c.cache_size(), 0, "expired entry must be swept on access");

        put_raw(
            &mut c,
            2,
            200,
            Some(Instant::now() + Duration::from_secs(60)),
        );
        assert_eq!(
            c.cache_get(&2),
            Some(&200),
            "now < expires_at must be a hit"
        );

        put_raw(&mut c, 3, 300, None);
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert_eq!(
            c.cache_get(&3),
            Some(&300),
            "expires_at = None never expires"
        );
    }

    #[test]
    fn cache_get_mut_boundary_matches_now_ge_expires_at_convention() {
        let mut c = long_ttl_cache(8);

        let tie = Instant::now();
        put_raw(&mut c, 1, 100, Some(tie));
        assert_eq!(
            c.cache_get_mut(&1),
            None,
            "tie (now >= expires_at) must be a miss"
        );
        assert_eq!(c.cache_size(), 0, "expired entry must be swept on access");

        put_raw(
            &mut c,
            2,
            200,
            Some(Instant::now() + Duration::from_secs(60)),
        );
        assert_eq!(
            c.cache_get_mut(&2),
            Some(&mut 200),
            "now < expires_at must be a hit"
        );

        put_raw(&mut c, 3, 300, None);
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert_eq!(
            c.cache_get_mut(&3),
            Some(&mut 300),
            "expires_at = None never expires"
        );
    }

    #[test]
    fn cache_set_boundary_matches_now_ge_expires_at_convention() {
        // `cache_set` samples `now` once and threads it into `set_entry`, which
        // decides whether the DISPLACED entry was still live.
        let mut c = long_ttl_cache(8);

        let tie = Instant::now();
        put_raw(&mut c, 1, 100, Some(tie));
        let before = c.cache_evictions().unwrap();
        assert_eq!(
            c.cache_set(1, 111),
            None,
            "displacing an entry at the tie (now >= expires_at) must return None"
        );
        assert_eq!(
            c.cache_evictions().unwrap(),
            before + 1,
            "the displaced expired entry must count as an eviction"
        );

        put_raw(
            &mut c,
            2,
            200,
            Some(Instant::now() + Duration::from_secs(60)),
        );
        let before = c.cache_evictions().unwrap();
        assert_eq!(
            c.cache_set(2, 222),
            Some(200),
            "displacing a live entry must return the old value"
        );
        assert_eq!(
            c.cache_evictions().unwrap(),
            before,
            "displacing a live entry must not count an eviction"
        );

        put_raw(&mut c, 3, 300, None);
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert_eq!(
            c.cache_set(3, 333),
            Some(300),
            "expires_at = None never expires, so the old value is returned"
        );
    }

    #[test]
    fn cache_get_or_set_with_boundary_matches_now_ge_expires_at_convention() {
        let mut c = long_ttl_cache(8);

        let tie = Instant::now();
        put_raw(&mut c, 1, 100, Some(tie));
        assert_eq!(
            *c.cache_get_or_set_with(1, || 999),
            999,
            "tie (now >= expires_at) must be treated as expired and replaced"
        );

        put_raw(
            &mut c,
            2,
            200,
            Some(Instant::now() + Duration::from_secs(60)),
        );
        assert_eq!(
            *c.cache_get_or_set_with(2, || 999),
            200,
            "now < expires_at must be a hit, so the factory must not run"
        );

        put_raw(&mut c, 3, 300, None);
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert_eq!(
            *c.cache_get_or_set_with(3, || 999),
            300,
            "expires_at = None never expires"
        );
    }

    #[test]
    fn cache_try_get_or_set_with_boundary_matches_now_ge_expires_at_convention() {
        let mut c = long_ttl_cache(8);

        let tie = Instant::now();
        put_raw(&mut c, 1, 100, Some(tie));
        assert_eq!(
            c.cache_try_get_or_set_with(1, || Ok::<u32, ()>(999))
                .copied(),
            Ok(999),
            "tie (now >= expires_at) must be treated as expired and replaced"
        );

        put_raw(
            &mut c,
            2,
            200,
            Some(Instant::now() + Duration::from_secs(60)),
        );
        assert_eq!(
            c.cache_try_get_or_set_with(2, || Ok::<u32, ()>(999))
                .copied(),
            Ok(200),
            "now < expires_at must be a hit, so the factory must not run"
        );

        put_raw(&mut c, 3, 300, None);
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert_eq!(
            c.cache_try_get_or_set_with(3, || Ok::<u32, ()>(999))
                .copied(),
            Ok(300),
            "expires_at = None never expires"
        );
    }

    #[test]
    fn cache_get_with_expiry_status_boundary_matches_now_ge_expires_at_convention() {
        let mut c = long_ttl_cache(8);

        let tie = Instant::now();
        put_raw(&mut c, 1, 100, Some(tie));
        assert_eq!(
            c.cache_get_with_expiry_status(&1),
            (Some(100), true),
            "tie (now >= expires_at) must report expired"
        );

        put_raw(
            &mut c,
            2,
            200,
            Some(Instant::now() + Duration::from_secs(60)),
        );
        assert_eq!(
            c.cache_get_with_expiry_status(&2),
            (Some(200), false),
            "now < expires_at must report live"
        );

        put_raw(&mut c, 3, 300, None);
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert_eq!(
            c.cache_get_with_expiry_status(&3),
            (Some(300), false),
            "expires_at = None never expires"
        );
    }

    #[test]
    fn order_collectors_boundary_matches_now_ge_expires_at_convention() {
        // `iter_order` / `key_order` / `value_order` hoist ONE clock reading for the
        // whole pass; the boundary they apply per entry must be unchanged.
        let mut c = long_ttl_cache(8);
        put_raw(&mut c, 1, 100, None); // never expires
        put_raw(
            &mut c,
            2,
            200,
            Some(Instant::now() + Duration::from_secs(60)),
        ); // live
        let tie = Instant::now();
        put_raw(&mut c, 3, 300, Some(tie)); // tie -> expired

        // MRU -> LRU is 3, 2, 1; the tie entry is filtered out of all three views.
        assert_eq!(c.key_order(), vec![2, 1]);
        assert_eq!(
            c.iter_order()
                .into_iter()
                .map(|(k, v)| (k, *v))
                .collect::<Vec<_>>(),
            vec![(2, 200), (1, 100)]
        );
        assert_eq!(
            c.value_order()
                .into_iter()
                .map(|v| v.into_value())
                .collect::<Vec<_>>(),
            vec![200, 100]
        );
        // The views are non-destructive: the expired entry is still stored.
        assert_eq!(c.cache_size(), 3);
    }

    #[test]
    fn evict_boundary_matches_now_ge_expires_at_convention() {
        let mut c = long_ttl_cache(8);
        put_raw(&mut c, 1, 100, None);
        put_raw(
            &mut c,
            2,
            200,
            Some(Instant::now() + Duration::from_secs(60)),
        );
        let tie = Instant::now();
        put_raw(&mut c, 3, 300, Some(tie));

        assert_eq!(c.evict(), 1, "only the tie entry is expired");
        assert_eq!(c.key_order(), vec![2, 1]);
    }

    #[test]
    fn retain_boundary_matches_now_ge_expires_at_convention() {
        let mut c = long_ttl_cache(8);
        put_raw(&mut c, 1, 100, None);
        put_raw(
            &mut c,
            2,
            200,
            Some(Instant::now() + Duration::from_secs(60)),
        );
        let tie = Instant::now();
        put_raw(&mut c, 3, 300, Some(tie));

        // Predicate keeps everything: only the expiry boundary decides.
        c.retain(|_k, _v| true);
        assert_eq!(
            c.key_order(),
            vec![2, 1],
            "the tie entry (now >= expires_at) must be swept regardless of the predicate"
        );
    }

    #[test]
    fn retain_returns_count_folding_expired_and_predicate_rejections() {
        // The returned count must fold together BOTH predicate-rejected entries and
        // entries removed because they had already expired, and must agree with the
        // `cache_size()` delta and the number of `on_evict` invocations.
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let fired = Arc::new(AtomicUsize::new(0));
        let fired2 = fired.clone();
        let mut c = LruTtlCache::builder()
            .max_size(10)
            .ttl(Duration::from_millis(30))
            .on_evict(move |_k: &u32, _v: &u32| {
                fired2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();

        // Key 1: will expire before the sweep, regardless of the predicate.
        c.cache_set(1, 10);
        std::thread::sleep(std::time::Duration::from_millis(80));
        // Keys 2-4: inserted after the sleep, still live relative to `retain`'s
        // hoisted `now`. Key 3 is rejected by the predicate, keys 2 and 4 are kept.
        c.cache_set(2, 20);
        c.cache_set(3, 31);
        c.cache_set(4, 40);

        let size_before = c.cache_size();
        let removed = c.retain(|_, v| v % 2 == 0);
        let size_after = c.cache_size();

        assert_eq!(
            removed, 2,
            "one expired sweep (key 1) + one predicate rejection (key 3)"
        );
        assert_eq!(size_before - size_after, removed);
        assert_eq!(fired.load(Ordering::Relaxed), removed);
        assert_eq!(c.cache_get(&2), Some(&20));
        assert_eq!(c.cache_get(&3), None);
        assert_eq!(c.cache_get(&4), Some(&40));
    }

    // --- refresh_on_hit anchoring ------------------------------------------

    // With `refresh_on_hit`, a hit must extend the entry's expiry by the FULL
    // configured ttl measured from the moment of the hit -- never by
    // ttl-minus-epsilon from a stale/earlier clock reading. Bracketed by clock
    // reads taken immediately before and after the hit.
    //
    // The ttl is deliberately far longer than the pre-hit sleep so a descheduled
    // test thread can never let the entry expire mid-test (that would turn a real
    // assertion failure into a flake); the sleep only has to be long enough that
    // `before + ttl` is strictly greater than the original expiry, which is what
    // makes "the expiry actually moved" observable.
    const REFRESH_TTL: Duration = Duration::from_secs(30);
    const REFRESH_GAP: std::time::Duration = std::time::Duration::from_millis(20);

    fn refreshing_cache() -> LruTtlCache<u32, u32> {
        LruTtlCache::builder()
            .max_size(4)
            .ttl(REFRESH_TTL)
            .refresh_on_hit(true)
            .build()
            .unwrap()
    }

    /// Assert that a hit bracketed by `before`/`after` re-anchored key 1's expiry to
    /// the hit itself, extended by the full ttl.
    fn assert_refreshed_to_hit_time(
        c: &LruTtlCache<u32, u32>,
        original: Instant,
        before: Instant,
        after: Instant,
    ) {
        let expires_at = stored_expiry(c, 1).expect("finite ttl carries an expiry");
        assert!(
            expires_at > original,
            "refresh_on_hit must move the expiry forward"
        );
        assert!(
            expires_at >= before + REFRESH_TTL,
            "refresh must extend by the FULL ttl measured from the hit, not less"
        );
        assert!(
            expires_at <= after + REFRESH_TTL,
            "refresh must not anchor to a clock reading taken before the hit"
        );
    }

    #[test]
    fn refresh_on_hit_cache_get_extends_by_full_ttl_from_hit_time() {
        let mut c = refreshing_cache();
        c.cache_set(1, 100);
        let original = stored_expiry(&c, 1).expect("finite ttl carries an expiry");
        std::thread::sleep(REFRESH_GAP);

        let before = Instant::now();
        assert_eq!(c.cache_get(&1), Some(&100));
        let after = Instant::now();

        assert_refreshed_to_hit_time(&c, original, before, after);
    }

    #[test]
    fn refresh_on_hit_cache_get_mut_extends_by_full_ttl_from_hit_time() {
        let mut c = refreshing_cache();
        c.cache_set(1, 100);
        let original = stored_expiry(&c, 1).expect("finite ttl carries an expiry");
        std::thread::sleep(REFRESH_GAP);

        let before = Instant::now();
        assert_eq!(c.cache_get_mut(&1), Some(&mut 100));
        let after = Instant::now();

        assert_refreshed_to_hit_time(&c, original, before, after);
    }

    #[test]
    fn refresh_on_hit_get_or_set_extends_by_full_ttl_from_hit_time() {
        // The hit path of `cache_get_or_set_with` now reuses the clock reading taken
        // by the store's validity check; it must still be a reading from THIS call.
        let mut c = refreshing_cache();
        c.cache_set(1, 100);
        let original = stored_expiry(&c, 1).expect("finite ttl carries an expiry");
        std::thread::sleep(REFRESH_GAP);

        let before = Instant::now();
        assert_eq!(*c.cache_get_or_set_with(1, || 999), 100);
        let after = Instant::now();

        assert_refreshed_to_hit_time(&c, original, before, after);
    }

    #[test]
    fn refresh_on_hit_try_get_or_set_extends_by_full_ttl_from_hit_time() {
        let mut c = refreshing_cache();
        c.cache_set(1, 100);
        let original = stored_expiry(&c, 1).expect("finite ttl carries an expiry");
        std::thread::sleep(REFRESH_GAP);

        let before = Instant::now();
        assert_eq!(
            c.cache_try_get_or_set_with(1, || Ok::<u32, ()>(999))
                .copied(),
            Ok(100)
        );
        let after = Instant::now();

        assert_refreshed_to_hit_time(&c, original, before, after);
    }

    #[test]
    fn refresh_on_hit_get_with_expiry_status_extends_by_full_ttl_from_hit_time() {
        let mut c = refreshing_cache();
        c.cache_set(1, 100);
        let original = stored_expiry(&c, 1).expect("finite ttl carries an expiry");
        std::thread::sleep(REFRESH_GAP);

        let before = Instant::now();
        assert_eq!(c.cache_get_with_expiry_status(&1), (Some(100), false));
        let after = Instant::now();

        assert_refreshed_to_hit_time(&c, original, before, after);
    }

    #[test]
    fn refresh_on_hit_disabled_leaves_expiry_untouched() {
        // Guard the other direction: reusing one clock reading must not start
        // refreshing entries in a cache that never opted into refresh_on_hit.
        let mut c: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_millis(400))
            .build()
            .unwrap();
        c.cache_set(1, 100);
        let original = stored_expiry(&c, 1).expect("finite ttl carries an expiry");
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert_eq!(c.cache_get(&1), Some(&100));
        assert_eq!(*c.cache_get_or_set_with(1, || 999), 100);
        assert_eq!(
            stored_expiry(&c, 1),
            Some(original),
            "without refresh_on_hit a hit must not move the expiry"
        );
    }

    // --- slow-factory anchoring ---------------------------------------------

    // The `hit_at` reading sampled for the validity check must NOT be reused as the
    // new entry's expiry anchor: the factory may run arbitrarily long, so the fresh
    // expiry has to be anchored AFTER it returns. Measured directly against the
    // stored `expires_at`, so it fails even if the factory is faster than the ttl.
    #[test]
    fn get_or_set_expiry_anchored_after_slow_factory_returns() {
        let ttl = Duration::from_millis(500);
        let mut c: LruTtlCache<u32, u32> =
            LruTtlCache::builder().max_size(4).ttl(ttl).build().unwrap();
        // Pre-seed an EXPIRED entry so the replacement path (validity check first,
        // then factory) is the one exercised.
        put_raw(&mut c, 1, 100, Some(Instant::now()));

        assert_eq!(
            *c.cache_get_or_set_with(1, || {
                std::thread::sleep(std::time::Duration::from_millis(120));
                7
            }),
            7
        );
        let factory_returned = Instant::now();
        let expires_at = stored_expiry(&c, 1).expect("finite ttl carries an expiry");
        assert!(
            expires_at + Duration::from_millis(120) > factory_returned + ttl,
            "the expiry must be anchored after the factory returned, not at lookup time"
        );
        assert_eq!(
            c.cache_get(&1),
            Some(&7),
            "entry must be live right after insert"
        );
    }

    #[test]
    fn try_get_or_set_expiry_anchored_after_slow_factory_returns() {
        let ttl = Duration::from_millis(500);
        let mut c: LruTtlCache<u32, u32> =
            LruTtlCache::builder().max_size(4).ttl(ttl).build().unwrap();
        put_raw(&mut c, 1, 100, Some(Instant::now()));

        assert_eq!(
            c.cache_try_get_or_set_with(1, || {
                std::thread::sleep(std::time::Duration::from_millis(120));
                Ok::<u32, ()>(7)
            })
            .copied(),
            Ok(7)
        );
        let factory_returned = Instant::now();
        let expires_at = stored_expiry(&c, 1).expect("finite ttl carries an expiry");
        assert!(
            expires_at + Duration::from_millis(120) > factory_returned + ttl,
            "the expiry must be anchored after the factory returned, not at lookup time"
        );
    }

    #[cfg(feature = "async")]
    #[tokio::test]
    async fn async_get_or_set_expiry_anchored_after_slow_factory_returns() {
        use crate::CachedGetOrSetAsync;
        let ttl = Duration::from_millis(500);
        let mut c: LruTtlCache<u32, u32> =
            LruTtlCache::builder().max_size(4).ttl(ttl).build().unwrap();
        put_raw(&mut c, 1, 100, Some(Instant::now()));

        let v = CachedGetOrSetAsync::async_cache_get_or_set_with(&mut c, 1, || async {
            tokio::time::sleep(std::time::Duration::from_millis(120)).await;
            7
        })
        .await;
        assert_eq!(*v, 7);
        let factory_returned = Instant::now();
        let expires_at = stored_expiry(&c, 1).expect("finite ttl carries an expiry");
        assert!(
            expires_at + Duration::from_millis(120) > factory_returned + ttl,
            "the expiry must be anchored after the factory resolved, not at lookup time"
        );
    }

    #[cfg(feature = "async")]
    #[tokio::test]
    async fn async_refresh_on_hit_extends_by_full_ttl_from_hit_time() {
        use crate::CachedGetOrSetAsync;
        let ttl = Duration::from_millis(200);
        let mut c: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(4)
            .ttl(ttl)
            .refresh_on_hit(true)
            .build()
            .unwrap();
        c.cache_set(1, 100);
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;

        let before = Instant::now();
        let v = CachedGetOrSetAsync::async_cache_get_or_set_with(&mut c, 1, || async { 999 }).await;
        assert_eq!(*v, 100);
        let after = Instant::now();

        let expires_at = stored_expiry(&c, 1).expect("finite ttl carries an expiry");
        assert!(
            expires_at >= before + ttl,
            "refresh must extend by the FULL ttl measured from the hit"
        );
        assert!(
            expires_at <= after + ttl,
            "refresh must not anchor to a clock reading taken before the hit"
        );
    }

    // --- one snapshot per eager sweep ---------------------------------------

    #[test]
    fn retain_judges_every_entry_against_one_pass_start_snapshot() {
        // `retain` samples the clock ONCE at the top of the pass. An entry that is
        // live when the pass starts must survive even if it expires while the pass
        // is still running. With a per-entry clock reading the slow predicate below
        // would push the second entry past its expiry and sweep it.
        let mut c = long_ttl_cache(8);
        // MRU -> LRU order is 2, 1: entry 1 is judged AFTER the slow predicate call
        // for entry 2.
        put_raw(
            &mut c,
            1,
            100,
            Some(Instant::now() + Duration::from_millis(80)),
        );
        put_raw(&mut c, 2, 200, None);

        c.retain(|_k, _v| {
            std::thread::sleep(std::time::Duration::from_millis(200));
            true
        });

        // `cache_size` is the RAW stored count (entry 1 has expired by now, so the
        // expiry-filtering `key_order` would not show it).
        assert_eq!(
            c.cache_size(),
            2,
            "an entry live at the start of the pass must survive the whole pass"
        );
    }

    #[test]
    fn evict_judges_every_entry_against_one_pass_start_snapshot() {
        // Same guarantee for `evict`, using a slow `on_evict` callback to stretch
        // the pass past a later entry's expiry.
        let mut c: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(8)
            .ttl(Duration::from_secs(60))
            .on_evict(|_k: &u32, _v: &u32| {
                std::thread::sleep(std::time::Duration::from_millis(200));
            })
            .build()
            .unwrap();
        // MRU -> LRU order is 3, 2, 1: the already-expired entry 2 fires the slow
        // callback before entry 1 is judged.
        put_raw(
            &mut c,
            1,
            100,
            Some(Instant::now() + Duration::from_millis(80)),
        );
        put_raw(&mut c, 2, 200, Some(Instant::now()));
        put_raw(&mut c, 3, 300, None);

        assert_eq!(
            c.evict(),
            1,
            "only the entry already expired at the start of the pass may be swept"
        );
        assert_eq!(c.cache_size(), 2);
    }

    // --- cache_clear_with_on_evict ------------------------------------------

    #[test]
    fn cache_clear_with_on_evict_fires_mru_to_lru() {
        use std::sync::Mutex;
        let fired: Arc<Mutex<Vec<(u32, u32)>>> = Arc::new(Mutex::new(Vec::new()));
        let fired2 = fired.clone();
        let mut c = LruTtlCache::builder()
            .max_size(5)
            .ttl(Duration::from_secs(60))
            .on_evict(move |k: &u32, v: &u32| {
                fired2.lock().unwrap().push((*k, *v));
            })
            .build()
            .unwrap();
        c.cache_set(1, 10);
        c.cache_set(2, 20);
        c.cache_set(3, 30);
        // Promote 1 so the MRU -> LRU order is 1, 3, 2 (not simply insertion order).
        assert_eq!(c.cache_get(&1), Some(&10));
        assert_eq!(c.key_order(), vec![1, 3, 2]);

        c.cache_clear_with_on_evict();

        assert_eq!(
            fired.lock().unwrap().as_slice(),
            &[(1, 10), (3, 30), (2, 20)],
            "on_evict must fire in MRU -> LRU order"
        );
        assert_eq!(c.cache_size(), 0);
    }

    #[test]
    fn cache_clear_with_on_evict_fires_for_expired_entries_too() {
        // The clear is expiry-blind: every stored entry fires the callback and is
        // counted, whether or not it had already expired.
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
        c.store.cache_set(
            1,
            TimedEntry {
                expires_at: Some(Instant::now()),
                value: 10,
            },
        );
        c.store.cache_set(
            2,
            TimedEntry {
                expires_at: None,
                value: 20,
            },
        );

        c.cache_clear_with_on_evict();
        assert_eq!(count.load(AtomicOrdering::Relaxed), 2);
        assert_eq!(c.evictions.load(AtomicOrdering::Relaxed), 2);
        assert_eq!(c.cache_size(), 0);
    }

    #[test]
    fn cache_clear_with_on_evict_leaves_cache_reusable() {
        // `drain_all` resets the LRU slab's sentinels; the cache must behave
        // normally afterwards (inserts, recency order, capacity eviction).
        let mut c = LruTtlCache::builder()
            .max_size(2)
            .ttl(Duration::from_secs(60))
            .on_evict(|_k: &u32, _v: &u32| {})
            .build()
            .unwrap();
        c.cache_set(1, 10);
        c.cache_set(2, 20);
        c.cache_clear_with_on_evict();
        assert_eq!(c.cache_size(), 0);
        assert_eq!(c.key_order(), Vec::<u32>::new());

        c.cache_set(3, 30);
        c.cache_set(4, 40);
        assert_eq!(c.key_order(), vec![4, 3]);
        assert_eq!(c.cache_get(&3), Some(&30));
        c.cache_set(5, 50); // evicts the LRU entry (4)
        assert_eq!(c.cache_size(), 2);
        assert_eq!(c.cache_get(&4), None);
        assert_eq!(c.cache_get(&3), Some(&30));
    }

    #[test]
    fn cache_clear_with_on_evict_counts_without_a_callback() {
        // The eviction count must not depend on whether a callback is configured:
        // attaching a purely observational `on_evict` cannot change `evictions`.
        // Matches `LruCache::cache_clear_with_on_evict`.
        let mut c = long_ttl_cache(4);
        c.cache_set(1, 10);
        c.cache_set(2, 20);
        let before = c.cache_evictions().unwrap();
        c.cache_clear_with_on_evict();
        assert_eq!(c.cache_size(), 0);
        assert_eq!(
            c.cache_evictions().unwrap(),
            before + 2,
            "clearing two entries counts two evictions with or without a callback"
        );

        // Same sequence, with a no-op callback: identical count.
        let mut with_cb: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_secs(60))
            .on_evict(|_: &u32, _: &u32| {})
            .build()
            .unwrap();
        with_cb.cache_set(1, 10);
        with_cb.cache_set(2, 20);
        with_cb.cache_clear_with_on_evict();
        assert_eq!(with_cb.cache_evictions(), c.cache_evictions());
    }

    // --- lazy expiry sweep reuses the probe's hash ---------------------------

    #[test]
    fn cache_get_lazy_sweep_removes_the_expired_entry() {
        // The expired branch pops with the hash already computed for the probe. A
        // mismatched hash would silently miss and leave the entry in the store.
        let count = Arc::new(AtomicUsize::new(0));
        let count2 = count.clone();
        let mut c = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_secs(60))
            .on_evict(move |_k: &u32, _v: &u32| {
                count2.fetch_add(1, AtomicOrdering::Relaxed);
            })
            .build()
            .unwrap();
        c.store.cache_set(
            1,
            TimedEntry {
                expires_at: Some(Instant::now()),
                value: 10,
            },
        );
        assert_eq!(c.cache_size(), 1);
        assert_eq!(c.cache_get(&1), None);
        assert_eq!(c.cache_size(), 0, "the expired entry must be removed");
        assert_eq!(count.load(AtomicOrdering::Relaxed), 1);
        assert_eq!(c.evictions.load(AtomicOrdering::Relaxed), 1);
        // A second get is a plain absent-key miss.
        assert_eq!(c.cache_get(&1), None);
        assert_eq!(count.load(AtomicOrdering::Relaxed), 1);
    }

    #[test]
    fn cache_get_mut_lazy_sweep_removes_the_expired_entry() {
        let mut c: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        c.store.cache_set(
            1,
            TimedEntry {
                expires_at: Some(Instant::now()),
                value: 10,
            },
        );
        assert_eq!(c.cache_get_mut(&1), None);
        assert_eq!(c.cache_size(), 0, "the expired entry must be removed");
    }

    #[test]
    fn cache_get_lazy_sweep_removes_borrowed_key_entry() {
        // Borrowed lookup form (`K = String`, `Q = str`): the hash reused by the
        // lazy sweep is the one computed from `&str`, which must still locate the
        // `String`-keyed entry.
        let mut c: LruTtlCache<String, u32> = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        c.store.cache_set(
            "alpha".to_string(),
            TimedEntry {
                expires_at: Some(Instant::now()),
                value: 10,
            },
        );
        assert_eq!(c.cache_get("alpha"), None);
        assert_eq!(
            c.cache_size(),
            0,
            "the expired entry must be removed via the borrowed-key hash"
        );

        // The live path over the same borrowed form still works.
        c.cache_set("beta".to_string(), 20);
        assert_eq!(c.cache_get("beta"), Some(&20));
        assert_eq!(c.cache_get_mut("beta"), Some(&mut 20));
    }

    // --- recency preserved ---------------------------------------------------

    #[test]
    fn clock_threading_does_not_change_cache_set_recency() {
        // `set_entry` goes through `cache_set_returning_entry`, which promotes the
        // overwritten key to MRU. Threading `now` through must not change that.
        let mut c = long_ttl_cache(3);
        c.cache_set(1, 10);
        c.cache_set(2, 20);
        c.cache_set(3, 30);
        assert_eq!(c.key_order(), vec![3, 2, 1]);
        assert_eq!(c.cache_set(1, 11), Some(10));
        assert_eq!(
            c.key_order(),
            vec![1, 3, 2],
            "an overwrite must promote the entry to MRU"
        );
        // ... and the get paths promote too.
        assert_eq!(c.cache_get(&2), Some(&20));
        assert_eq!(c.key_order(), vec![2, 1, 3]);
        // Overwriting an EXPIRED entry (the other `set_entry` arm) promotes as well;
        // the displaced expired value is filtered from the return.
        // (`put_raw` writes through the inner store, so it promotes too; `key_order`
        // filters the now-expired key 3 out of the visible order.)
        put_raw(&mut c, 3, 33, Some(Instant::now()));
        assert_eq!(c.key_order(), vec![2, 1]);
        assert_eq!(c.cache_set(3, 333), None);
        assert_eq!(c.key_order(), vec![3, 2, 1]);
    }

    // --- peek_expires_at -------------------------------------------------------

    #[test]
    fn peek_expires_at_absent_key_returns_none_none() {
        let c: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        assert_eq!(c.cache_peek_expires_at(&1u32), (None, None));
        assert_eq!(c.peek_expires_at(&1u32), (None, None));
    }

    #[test]
    fn peek_expires_at_live_entry_returns_the_stored_future_deadline() {
        let ttl = Duration::from_secs(60);
        let mut c: LruTtlCache<u32, u32> =
            LruTtlCache::builder().max_size(4).ttl(ttl).build().unwrap();
        let before = Instant::now();
        c.cache_set(1, 100);
        let after = Instant::now();

        let stored = stored_expiry(&c, 1).expect("a configured ttl must record a deadline");

        let (value, expires_at) = c.cache_peek_expires_at(&1u32);
        assert_eq!(value, Some(100));
        assert_eq!(
            expires_at,
            Some(stored),
            "the reported deadline must be the one the store holds"
        );
        let expires_at = expires_at.unwrap();
        assert!(expires_at > Instant::now(), "a live entry expires later");
        assert!(expires_at >= before + ttl && expires_at <= after + ttl);
    }

    #[test]
    fn peek_expires_at_never_expiring_entry_reports_no_deadline() {
        use crate::CacheTtl;
        let mut c: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        // A zero ttl disables expiry, so the entry is stored without a deadline.
        c.unset_ttl();
        c.cache_set(1, 100);
        assert_eq!(c.cache_peek_expires_at(&1u32), (Some(100), None));
        // Distinguishable from an absent key by the value, not by the deadline.
        assert_eq!(c.cache_peek_with_expiry_status(&1u32), (Some(100), false));
    }

    #[test]
    fn peek_expires_at_expired_entry_returns_a_past_deadline_and_keeps_the_entry() {
        let mut c: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_millis(20))
            .build()
            .unwrap();
        c.cache_set(1, 100);
        std::thread::sleep(std::time::Duration::from_millis(60));

        let (value, expires_at) = c.cache_peek_expires_at(&1u32);
        assert_eq!(value, Some(100), "an expired entry is still returned");
        let expires_at = expires_at.expect("an expired entry still carries its deadline");
        assert!(expires_at <= Instant::now(), "the deadline is in the past");
        // Not removed by the peek: a second peek sees the same entry and deadline, and
        // the raw entry count (which includes not-yet-swept expired entries) is unchanged.
        assert_eq!(c.cache_size(), 1);
        assert_eq!(
            c.cache_peek_expires_at(&1u32),
            (Some(100), Some(expires_at))
        );
    }

    #[test]
    fn peek_expires_at_deadline_is_past_exactly_when_peek_reports_expired() {
        let mut c: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_millis(20))
            .build()
            .unwrap();
        c.cache_set(1, 100);
        for _ in 0..2 {
            let (_, expires_at) = c.cache_peek_expires_at(&1u32);
            let (_, expired) = c.cache_peek_with_expiry_status(&1u32);
            assert_eq!(
                expires_at.is_some_and(|t| t <= Instant::now()),
                expired,
                "the deadline must be in the past exactly when the peek reports expired"
            );
            std::thread::sleep(std::time::Duration::from_millis(60));
        }
    }

    #[test]
    fn peek_expires_at_does_not_touch_hit_or_miss_counters() {
        let mut c: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        c.cache_set(1, 100);
        let hits = c.cache_hits();
        let misses = c.cache_misses();

        let _ = c.cache_peek_expires_at(&1u32); // present
        let _ = c.cache_peek_expires_at(&2u32); // absent

        assert_eq!(c.cache_hits(), hits, "a peek must not count a hit");
        assert_eq!(c.cache_misses(), misses, "a peek must not count a miss");
    }

    #[test]
    fn peek_expires_at_does_not_renew_the_ttl_with_refresh_on_hit() {
        use crate::CacheRefreshOnHit;
        let mut c: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_millis(200))
            .build()
            .unwrap();
        c.set_refresh_on_hit(true);
        c.cache_set(1, 100);

        let (_, first) = c.cache_peek_expires_at(&1u32);
        std::thread::sleep(std::time::Duration::from_millis(40));
        let (_, second) = c.cache_peek_expires_at(&1u32);
        assert_eq!(
            first, second,
            "peeking must not renew the ttl even with refresh_on_hit enabled"
        );

        // Control: a real hit does renew, so the assertion above is not vacuous.
        assert_eq!(c.cache_get(&1u32), Some(&100));
        let (_, after_hit) = c.cache_peek_expires_at(&1u32);
        assert!(
            after_hit > first,
            "refresh_on_hit must extend the deadline on a real read"
        );
    }

    #[test]
    fn peek_expires_at_does_not_promote_recency() {
        let mut c: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(3)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        c.cache_set(1, 10);
        c.cache_set(2, 20);
        c.cache_set(3, 30);
        assert_eq!(c.key_order(), vec![3, 2, 1]);

        // Peeking a non-MRU key must leave the recency order untouched.
        assert_eq!(c.cache_peek_expires_at(&1u32).0, Some(10));
        assert_eq!(
            c.key_order(),
            vec![3, 2, 1],
            "peek_expires_at must not promote"
        );

        // Control: a real hit on the same key DOES promote, so the assertion above
        // is not vacuous.
        assert_eq!(c.cache_get(&1u32), Some(&10));
        assert_eq!(c.key_order(), vec![1, 3, 2]);
    }

    /// Strengthens `peek_expires_at_does_not_promote_recency`: a `key_order()` listing could
    /// in principle be wrong while the real LRU chain is right (or vice versa). Drive the
    /// cache past `max_size` after the peek and confirm the peeked entry is actually the one
    /// physically evicted -- proof through the store's real eviction behavior, not just an
    /// order-listing helper.
    #[test]
    fn peek_expires_at_leaves_the_peeked_entry_as_the_next_eviction_victim() {
        let mut c: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(3)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        c.cache_set(1, 10);
        c.cache_set(2, 20);
        c.cache_set(3, 30);
        // LRU tail is key 1.
        assert_eq!(c.key_order(), vec![3, 2, 1]);

        // Peeking the tail key must not save it from being the next eviction victim.
        assert_eq!(c.cache_peek_expires_at(&1u32).0, Some(10));

        // Push past capacity: if the peek had (incorrectly) promoted key 1, key 2 would be
        // evicted instead.
        c.cache_set(4, 40);
        assert_eq!(
            c.cache_peek_expires_at(&1u32),
            (None, None),
            "the peeked-but-not-promoted key must be the one physically evicted"
        );
        assert_eq!(
            c.cache_peek_expires_at(&2u32).0,
            Some(20),
            "key 2 must have survived -- it was never the LRU victim"
        );
        assert_eq!(c.key_order(), vec![4, 3, 2]);
    }

    /// Gap: peeking an expired-but-not-yet-swept entry, then physically sweeping it with
    /// `evict()`, must transition the peek's view from "present with a past deadline" to
    /// fully absent -- `evict()` must not somehow leave the peek's answer stale.
    #[test]
    fn peek_expires_at_reports_absent_after_evict_sweeps_the_expired_entry() {
        let mut c: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_millis(20))
            .build()
            .unwrap();
        c.cache_set(1, 100);
        std::thread::sleep(std::time::Duration::from_millis(60));

        // Before the sweep: present, expired, not removed.
        let (value, expires_at) = c.cache_peek_expires_at(&1u32);
        assert_eq!(value, Some(100));
        assert!(expires_at.is_some());
        assert_eq!(c.cache_size(), 1);

        assert_eq!(c.evict(), 1, "the expired entry must be swept");
        assert_eq!(c.cache_size(), 0);
        assert_eq!(
            c.cache_peek_expires_at(&1u32),
            (None, None),
            "after evict() physically removes the entry, peek must report absent"
        );
    }

    /// Gap: `retain()` removes expired entries unconditionally (regardless of the predicate)
    /// while leaving live entries the predicate keeps untouched. A peek before/after must
    /// track that split exactly: absent for the swept expired key, unchanged for the
    /// surviving live key.
    #[test]
    fn peek_expires_at_interleaved_with_retain_tracks_the_sweep() {
        let mut c: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_millis(20))
            .build()
            .unwrap();
        c.cache_set(1, 100);
        std::thread::sleep(std::time::Duration::from_millis(60));
        c.cache_set(2, 200);

        // Key 1 has expired but is not yet swept; key 2 is live.
        assert!(c.cache_peek_expires_at(&1u32).1.is_some());
        let (_, live_before) = c.cache_peek_expires_at(&2u32);
        assert!(live_before.is_some());

        // `keep` always returns true: only the already-expired entry (key 1) is removed.
        let removed = c.retain(|_k, _v| true);
        assert_eq!(removed, 1);

        assert_eq!(
            c.cache_peek_expires_at(&1u32),
            (None, None),
            "retain must have swept the expired key"
        );
        assert_eq!(
            c.cache_peek_expires_at(&2u32),
            (Some(200), live_before),
            "retain must leave the live, kept key's peek view unchanged"
        );
    }

    /// Gap: the defaulted `peek_expires_at` alias must agree with `cache_peek_expires_at`
    /// across every return shape the contract defines, not just the absent-key case the
    /// existing alias test covers.
    #[test]
    fn peek_expires_at_alias_agrees_with_primary_across_all_shapes() {
        use crate::CacheTtl;
        let mut c: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_millis(20))
            .build()
            .unwrap();

        // Live.
        c.cache_set(1, 100);
        assert_eq!(c.peek_expires_at(&1u32), c.cache_peek_expires_at(&1u32));
        let (_, live_deadline) = c.cache_peek_expires_at(&1u32);
        assert!(live_deadline.is_some());

        // Never-expiring.
        c.unset_ttl();
        c.cache_set(2, 200);
        assert_eq!(c.peek_expires_at(&2u32), c.cache_peek_expires_at(&2u32));
        assert_eq!(c.cache_peek_expires_at(&2u32), (Some(200), None));

        // Expired.
        c.set_ttl(Duration::from_millis(20));
        c.cache_set(3, 300);
        std::thread::sleep(std::time::Duration::from_millis(60));
        assert_eq!(c.peek_expires_at(&3u32), c.cache_peek_expires_at(&3u32));
        let (expired_value, expired_deadline) = c.cache_peek_expires_at(&3u32);
        assert_eq!(expired_value, Some(300));
        assert!(expired_deadline.is_some_and(|t| t <= Instant::now()));
    }

    /// Cross-store consistency: an extreme TTL that overflows `Instant::checked_add`
    /// (see `compute_expires_at`) must be reported by `cache_peek_expires_at` as
    /// never-expiring, exactly like `cache_set` / `iter_order` already pin. Companion to
    /// `cache_set_with_ttl_overflow_stores_never_expiring_entry`, but exercised through the
    /// `CacheExpiry` read path.
    #[test]
    fn peek_expires_at_reports_no_deadline_under_ttl_overflow() {
        let mut c: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::MAX)
            .build()
            .unwrap();
        c.cache_set(1, 42);
        assert_eq!(
            c.cache_peek_expires_at(&1u32),
            (Some(42), None),
            "a TTL that overflows Instant::checked_add must peek as never-expiring"
        );
    }

    /// Pins the exact raw passthrough of `expires_at` at the `now == expires_at` boundary:
    /// `cache_peek_expires_at` must return the untouched stored tie value, not merely
    /// something `<=` the current clock. The tie-sensitive agreement with
    /// `cache_peek_with_expiry_status`'s expired flag is already covered by
    /// `peek_expires_at_deadline_is_past_exactly_when_peek_reports_expired`.
    #[test]
    fn peek_expires_at_boundary_matches_now_ge_expires_at_convention() {
        let mut c = long_ttl_cache(8);
        let tie = Instant::now();
        put_raw(&mut c, 1, 100, Some(tie));

        let (value, expires_at) = c.cache_peek_expires_at(&1u32);
        assert_eq!(value, Some(100));
        assert_eq!(expires_at, Some(tie));
        assert!(
            expires_at.is_some_and(|t| t <= Instant::now()),
            "a tie (now >= expires_at) must be reported as already past"
        );
        assert_eq!(
            c.cache_peek_with_expiry_status(&1u32),
            (Some(100), true),
            "the tie must also be flagged expired by the sibling peek method"
        );
    }

    /// Gap: `set_ttl` / `unset_ttl` change the store-wide TTL used for *future* writes; an
    /// already-stored entry's own deadline must not retroactively change. `peek_expires_at`
    /// reads the entry's own field, so it must keep reporting the original deadline
    /// through both a `set_ttl` bump and an `unset_ttl` call.
    #[test]
    fn peek_expires_at_ignores_later_set_ttl_and_unset_ttl_changes() {
        use crate::CacheTtl;
        let mut c: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_millis(100))
            .build()
            .unwrap();
        c.cache_set(1, 100);
        let (_, original_deadline) = c.cache_peek_expires_at(&1u32);
        assert!(original_deadline.is_some());

        // Growing the store-wide TTL must not extend the already-stored entry's deadline.
        c.set_ttl(Duration::from_secs(600));
        assert_eq!(
            c.cache_peek_expires_at(&1u32),
            (Some(100), original_deadline),
            "set_ttl must not retroactively change an existing entry's deadline"
        );

        // Disabling expiry for future writes must not clear the existing entry's deadline.
        c.unset_ttl();
        assert_eq!(
            c.cache_peek_expires_at(&1u32),
            (Some(100), original_deadline),
            "unset_ttl must not retroactively clear an existing entry's deadline"
        );
    }

    // --- CacheExpiry::cache_expires_at (the value-free read) -------------------

    #[test]
    fn expires_at_absent_key_returns_false_none() {
        let c: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        assert_eq!(c.cache_expires_at(&1u32), (false, None));
        assert_eq!(c.expires_at(&1u32), (false, None));
    }

    #[test]
    fn expires_at_live_entry_returns_the_stored_future_deadline() {
        let ttl = Duration::from_secs(60);
        let mut c: LruTtlCache<u32, u32> =
            LruTtlCache::builder().max_size(4).ttl(ttl).build().unwrap();
        let before = Instant::now();
        c.cache_set(1, 100);
        let after = Instant::now();

        let stored = stored_expiry(&c, 1).expect("a configured ttl must record a deadline");

        let (present, expires_at) = c.cache_expires_at(&1u32);
        assert!(present, "a stored key must report present");
        assert_eq!(
            expires_at,
            Some(stored),
            "the reported deadline must be the one the store holds"
        );
        let expires_at = expires_at.unwrap();
        assert!(expires_at > Instant::now(), "a live entry expires later");
        assert!(expires_at >= before + ttl && expires_at <= after + ttl);
    }

    #[test]
    fn expires_at_never_expiring_entry_reports_present_with_no_deadline() {
        use crate::CacheTtl;
        let mut c: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        // A zero ttl disables expiry, so the entry is stored without a deadline.
        c.unset_ttl();
        c.cache_set(1, 100);
        assert_eq!(
            c.cache_expires_at(&1u32),
            (true, None),
            "present-with-no-deadline must be distinguishable from absent by the flag"
        );
        assert_eq!(c.cache_expires_at(&2u32), (false, None));
    }

    #[test]
    fn expires_at_expired_entry_returns_a_past_deadline_and_keeps_the_entry() {
        let mut c: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_millis(20))
            .build()
            .unwrap();
        c.cache_set(1, 100);
        std::thread::sleep(std::time::Duration::from_millis(60));

        let (present, expires_at) = c.cache_expires_at(&1u32);
        assert!(present, "an expired entry is still reported present");
        let expires_at = expires_at.expect("an expired entry still carries its deadline");
        assert!(expires_at <= Instant::now(), "the deadline is in the past");
        // Not removed by the read: a second read sees the same entry and deadline.
        assert_eq!(c.cache_size(), 1);
        assert_eq!(c.cache_expires_at(&1u32), (true, Some(expires_at)));
    }

    // The two reads must never disagree: same deadline, and the presence flag must track
    // whether the value-bearing read returned `Some`.
    #[test]
    fn expires_at_agrees_with_peek_expires_at_across_all_return_shapes() {
        use crate::CacheTtl;
        let mut c: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_millis(20))
            .build()
            .unwrap();

        let check = |c: &LruTtlCache<u32, u32>, k: u32, label: &str| {
            let (value, peeked) = c.cache_peek_expires_at(&k);
            let (present, deadline) = c.cache_expires_at(&k);
            assert_eq!(
                present,
                value.is_some(),
                "presence flag disagrees ({label})"
            );
            assert_eq!(deadline, peeked, "deadline disagrees ({label})");
            assert_eq!(
                c.expires_at(&k),
                c.cache_expires_at(&k),
                "alias disagrees ({label})"
            );
            (present, deadline)
        };

        // absent
        assert_eq!(check(&c, 1, "absent"), (false, None));

        // live
        c.cache_set(1, 100);
        let (present, deadline) = check(&c, 1, "live");
        assert!(present && deadline.unwrap() > Instant::now());

        // expired, not removed
        std::thread::sleep(std::time::Duration::from_millis(60));
        let (present, deadline) = check(&c, 1, "expired");
        assert!(present && deadline.unwrap() <= Instant::now());

        // never-expiring
        c.unset_ttl();
        c.cache_set(2, 200);
        assert_eq!(check(&c, 2, "never-expiring"), (true, None));
    }

    #[test]
    fn expires_at_does_not_touch_hit_or_miss_counters() {
        let mut c: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        c.cache_set(1, 100);
        let hits = c.cache_hits();
        let misses = c.cache_misses();

        let _ = c.cache_expires_at(&1u32); // present
        let _ = c.cache_expires_at(&2u32); // absent
        let _ = c.expires_at(&1u32); // through the alias

        assert_eq!(c.cache_hits(), hits, "the read must not count a hit");
        assert_eq!(c.cache_misses(), misses, "the read must not count a miss");
    }

    #[test]
    fn expires_at_does_not_renew_the_ttl_with_refresh_on_hit() {
        use crate::CacheRefreshOnHit;
        let mut c: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_millis(200))
            .build()
            .unwrap();
        c.set_refresh_on_hit(true);
        c.cache_set(1, 100);

        let (_, first) = c.cache_expires_at(&1u32);
        std::thread::sleep(std::time::Duration::from_millis(40));
        let (_, second) = c.cache_expires_at(&1u32);
        assert_eq!(
            first, second,
            "the read must not renew the ttl even with refresh_on_hit enabled"
        );

        // Control: a real hit does renew, so the assertion above is not vacuous.
        assert_eq!(c.cache_get(&1u32), Some(&100));
        let (_, after_hit) = c.cache_expires_at(&1u32);
        assert!(
            after_hit > first,
            "refresh_on_hit must extend the deadline on a real read"
        );
    }

    #[test]
    fn expires_at_does_not_promote_recency() {
        let mut c: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(3)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        c.cache_set(1, 10);
        c.cache_set(2, 20);
        c.cache_set(3, 30);
        assert_eq!(c.key_order(), vec![3, 2, 1]);

        // Reading a non-MRU key's deadline must leave the recency order untouched.
        assert!(c.cache_expires_at(&1u32).0);
        assert_eq!(
            c.key_order(),
            vec![3, 2, 1],
            "cache_expires_at must not promote"
        );

        // Control: a real hit on the same key DOES promote, so the assertion above
        // is not vacuous.
        assert_eq!(c.cache_get(&1u32), Some(&10));
        assert_eq!(c.key_order(), vec![1, 3, 2]);
    }

    /// Strengthens `expires_at_does_not_promote_recency` behaviorally: after reading the
    /// LRU-tail key's deadline, force a capacity eviction and assert that same key is the
    /// one physically evicted -- proof through the store's real eviction behavior, not just
    /// an order-listing helper.
    #[test]
    fn expires_at_leaves_the_read_entry_as_the_next_eviction_victim() {
        let mut c: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(3)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        c.cache_set(1, 10);
        c.cache_set(2, 20);
        c.cache_set(3, 30);
        // LRU tail is key 1.
        assert_eq!(c.key_order(), vec![3, 2, 1]);

        // Reading the tail key's deadline must not save it from being the next eviction victim.
        assert!(c.cache_expires_at(&1u32).0);

        // Push past capacity: if the read had (incorrectly) promoted key 1, key 2 would be
        // evicted instead.
        c.cache_set(4, 40);
        assert_eq!(
            c.cache_expires_at(&1u32),
            (false, None),
            "the read-but-not-promoted key must be the one physically evicted"
        );
        assert!(
            c.cache_expires_at(&2u32).0,
            "key 2 must have survived -- it was never the LRU victim"
        );
        assert_eq!(c.key_order(), vec![4, 3, 2]);
    }

    // The point of moving `V: Clone` off the impl block and onto the value-bearing methods: a
    // deadline read must work on a cache whose value type is not `Clone` at all. The generic
    // helper carries no `V: Clone` bound anywhere, so this fails to compile if the bound
    // creeps back onto either the trait method or the impl.
    #[test]
    fn expires_at_reads_a_deadline_for_a_value_type_that_is_not_clone() {
        #[derive(Debug, PartialEq)]
        struct NotClone(u32);

        fn deadline<K: Hash + Eq + Clone, V>(
            c: &LruTtlCache<K, V>,
            k: &K,
        ) -> (bool, Option<Instant>) {
            c.cache_expires_at(k)
        }

        let ttl = Duration::from_secs(60);
        let mut c: LruTtlCache<u32, NotClone> =
            LruTtlCache::builder().max_size(4).ttl(ttl).build().unwrap();
        c.cache_set(1, NotClone(100));

        let (present, expires_at) = deadline(&c, &1);
        assert!(present);
        assert!(
            expires_at.expect("a configured ttl must record a deadline") > Instant::now(),
            "a live entry expires later"
        );
        assert_eq!(deadline(&c, &2), (false, None), "absent key");
        // The alias is equally bound-free.
        assert!(c.expires_at(&1u32).0);
        // The value was never cloned or moved out: it is still in the store.
        assert_eq!(c.cache_get(&1u32), Some(&NotClone(100)));
    }

    // The `now >= t` tie convention: the deadline the value-free read reports must be judged
    // expired at exactly the instant `cache_peek_with_expiry_status` calls the entry expired.
    #[test]
    fn expires_at_boundary_matches_now_ge_expires_at_convention() {
        let mut c = long_ttl_cache(8);
        let tie = Instant::now();
        put_raw(&mut c, 1, 100, Some(tie));

        assert_eq!(c.cache_expires_at(&1u32), (true, Some(tie)));
        assert_eq!(
            c.cache_peek_with_expiry_status(&1u32),
            (Some(100), true),
            "now == expires_at must be treated as expired, matching the now >= expires_at convention"
        );
    }

    // Physical removal must be reflected as absent, not as present-with-no-deadline.
    #[test]
    fn expires_at_reports_absent_after_removal() {
        let mut c: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_millis(20))
            .build()
            .unwrap();
        c.cache_set(1, 100);
        std::thread::sleep(std::time::Duration::from_millis(60));

        // Expired but not yet swept: still present, with a past deadline.
        let (present, expires_at) = c.cache_expires_at(&1u32);
        assert!(present);
        assert!(expires_at.unwrap() <= Instant::now());

        assert_eq!(c.evict(), 1, "evict must remove the expired entry");
        assert_eq!(
            c.cache_expires_at(&1u32),
            (false, None),
            "a physically removed entry must be reported absent"
        );

        c.cache_set(2, 200);
        assert_eq!(c.cache_remove(&2u32), Some(200));
        assert_eq!(c.cache_expires_at(&2u32), (false, None));
    }

    // Generic bounds, so these can only reach the trait methods: the inherent methods of the
    // same name win at a concrete call site.
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
    fn set_max_size_through_trait_shrinks_eagerly_and_fires_on_evict() {
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
        assert_eq!(cache.cache_get(&1), Some(&10));
        assert_eq!(cache.cache_get(&2), Some(&20));

        assert_eq!(resize_through_trait(&mut cache, 2), Some(4));
        assert_eq!(cache.capacity(), 2);
        // Eviction happens before the call returns, not on the next insert.
        assert_eq!(cache.cache_size(), 2);
        assert_eq!(cache.cache_evictions(), Some(2));
        assert_eq!(*evicted_keys.lock().unwrap(), vec![3, 4]);
        assert_eq!(cache.cache_get(&1), Some(&10));
        assert_eq!(cache.cache_get(&2), Some(&20));
        assert_eq!(cache.cache_get(&3), None);

        // Growing back reports the shrunk bound and keeps the survivors.
        assert_eq!(resize_through_trait(&mut cache, 8), Some(2));
        assert_eq!(cache.capacity(), 8);
        assert_eq!(cache.cache_size(), 2);
    }

    #[test]
    fn try_set_max_size_through_trait_rejects_zero() {
        let mut cache: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        assert_eq!(
            try_resize_through_trait(&mut cache, 0),
            Err(crate::SetMaxSizeError::ZeroMaxSize)
        );
        assert_eq!(cache.capacity(), 4);
        assert_eq!(try_resize_through_trait(&mut cache, 2), Ok(Some(4)));
        assert_eq!(cache.capacity(), 2);
    }

    #[test]
    fn cache_clear_with_on_evict_through_trait_fires_for_all_entries() {
        let evicted = Arc::new(AtomicUsize::new(0));
        let evicted2 = evicted.clone();
        let mut cache = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_secs(60))
            .on_evict(move |_k: &u32, _v: &u32| {
                evicted2.fetch_add(1, AtomicOrdering::Relaxed);
            })
            .build()
            .unwrap();
        cache.cache_set(1, 10);
        cache.cache_set(2, 20);
        cache.cache_set(3, 30);

        clear_with_on_evict_through_trait(&mut cache);
        assert_eq!(cache.cache_size(), 0);
        assert_eq!(evicted.load(AtomicOrdering::Relaxed), 3);
        assert_eq!(cache.cache_evictions(), Some(3));
    }
}
