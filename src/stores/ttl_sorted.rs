use crate::time::Duration;
use crate::time::Instant;
use crate::{CacheEvict, CacheTtl, Cached, CachedIter, CachedPeek, CachedRead, CloneCached};

use super::{DefaultHashBuilder, StripedCounter};
use std::borrow::Borrow;
use std::cmp::Ordering as CmpOrdering;
use std::collections::BTreeSet;
use std::hash::{BuildHasher, Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
#[cfg(feature = "async_core")]
use {super::CachedGetOrSetAsync, std::future::Future};

use std::collections::HashMap;

/// Wrap keys in Arc for shared ownership between the HashMap values and BTreeSet index.
#[derive(Eq)]
struct CacheArc<T>(Arc<T>);

impl<T> CacheArc<T> {
    fn new(key: T) -> Self {
        CacheArc(Arc::new(key))
    }
}

impl<T> Clone for CacheArc<T> {
    fn clone(&self) -> Self {
        CacheArc(self.0.clone())
    }
}

impl<T: PartialEq> PartialEq for CacheArc<T> {
    fn eq(&self, other: &Self) -> bool {
        self.0.eq(&other.0)
    }
}

impl<T: PartialOrd> PartialOrd for CacheArc<T> {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
        self.0.partial_cmp(&other.0)
    }
}
impl<T: Ord> Ord for CacheArc<T> {
    fn cmp(&self, other: &Self) -> CmpOrdering {
        self.0.cmp(&other.0)
    }
}

impl<T: Hash> Hash for CacheArc<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

impl<T> Borrow<T> for CacheArc<T> {
    fn borrow(&self) -> &T {
        &self.0
    }
}

/// A timestamped key to allow identifying key ranges.
///
/// `expiry` is `Option<Instant>`: `None` means "never expires" and sorts as GREATER
/// than any `Some(instant)` so that never-expiring entries appear last in the
/// expiry-ordered BTreeSet (evicted last under size pressure, never swept by TTL).
/// Rust's default `Option` ordering would put `None` first (least), so we implement
/// a custom `Ord` / `PartialOrd` that reverses that.
#[derive(Hash, Eq, PartialEq)]
struct Stamped<K> {
    expiry: Option<Instant>,

    // wrapped in an option so it's easy to generate
    // a range bound containing None
    key: Option<CacheArc<K>>,
}

impl<K: Ord> Ord for Stamped<K> {
    fn cmp(&self, other: &Self) -> CmpOrdering {
        // Compare expiries: None (never-expires) sorts GREATEST.
        let expiry_ord = match (&self.expiry, &other.expiry) {
            (None, None) => CmpOrdering::Equal,
            (None, Some(_)) => CmpOrdering::Greater,
            (Some(_), None) => CmpOrdering::Less,
            (Some(a), Some(b)) => a.cmp(b),
        };
        expiry_ord.then_with(|| self.key.cmp(&other.key))
    }
}

impl<K: Ord> PartialOrd for Stamped<K> {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(other))
    }
}

impl<K> Clone for Stamped<K> {
    fn clone(&self) -> Self {
        Self {
            expiry: self.expiry,
            key: self.key.clone(),
        }
    }
}

impl<K> Stamped<K> {
    /// Build a sentinel `Stamped` for use as a BTreeSet range bound.
    /// Only `Some(expiry)` bounds are used for expiry-sweep ranges; never-expiring
    /// entries (`None`) sort beyond all `Some(_)` values and are excluded automatically.
    ///
    /// This is the canonical constructor for the `key: None` sentinel that the `Option` in
    /// [`Stamped::key`] exists for: [`TtlSortedCache::evict_at`] uses it as the `split_off`
    /// pivot, and the tests use it to re-derive the pre-refactor range-based expected counts.
    fn bound(expiry: Instant) -> Stamped<K> {
        Stamped {
            expiry: Some(expiry),
            key: None,
        }
    }
}

/// A timestamped value to allow re-building a timestamped key.
/// `expiry` is `None` when the entry never expires (TTL was zero at insert time).
struct Entry<K, V> {
    expiry: Option<Instant>,
    key: CacheArc<K>,
    value: V,
}

impl<K, V> Entry<K, V> {
    fn as_stamped(&self) -> Stamped<K> {
        Stamped {
            expiry: self.expiry,
            key: Some(self.key.clone()),
        }
    }

    /// Returns `true` if the entry's expiry instant has passed as of `now`.
    ///
    /// Expiry is at-or-after the deadline (`now >= expiry`), matching
    /// [`TtlCache`](super::TtlCache) / `LruTtlCache`. Split from [`is_expired`](Self::is_expired)
    /// so the boundary can be unit-tested with a controlled `now` (the real monotonic clock
    /// never yields an exact `now == expiry` tie deterministically).
    fn is_expired_at(&self, now: Instant) -> bool {
        self.expiry.is_some_and(|e| e <= now)
    }

    /// Returns `true` if the entry's expiry instant has passed as of the current time.
    fn is_expired(&self) -> bool {
        self.is_expired_at(Instant::now())
    }
}

impl<K, V: Clone> Clone for Entry<K, V> {
    fn clone(&self) -> Self {
        Self {
            expiry: self.expiry,
            key: self.key.clone(),
            value: self.value.clone(),
        }
    }
}

/// A cache enforcing time expiration and an optional maximum size.
/// When a maximum size is specified, the values are dropped in the
/// order of expiration date, e.g. the next value to expire is dropped.
/// This cache is intended for high read scenarios to allow for concurrent
/// reads while still enforcing expiration and an optional maximum cache size.
///
/// To accomplish this, there are a few trade-offs:
///  - Maximum cache size logic cannot support "LRU", instead dropping the next value to expire
///  - Cache keys must implement `Ord`
///  - Eviction must be explicitly requested, either on its own or while inserting
///
/// **`len` / `iter` / `evict` contract**: `len()` returns the raw stored entry count
/// and may include expired-but-not-yet-swept entries - it is only guaranteed to be
/// accurate immediately after a call to `evict()` or `retain_latest()`. `iter()` omits
/// expired entries from the view but does not remove them. Call `evict()` (via
/// [`CacheEvict`](crate::CacheEvict)) to physically remove expired entries and obtain
/// an accurate live count.
///
/// `cache_get_or_set_with` returns `&V` (a shared reference), not `&mut V`.
/// Binding it as `&mut V` is a compile error; use
/// [`cache_get_or_set_with_mut`](crate::Cached::cache_get_or_set_with_mut) when
/// a mutable reference is needed.
///
/// ```compile_fail
/// use cached::{Cached, stores::TtlSortedCache};
/// use cached::time::Duration;
///
/// let mut cache = TtlSortedCache::<u32, u32>::builder()
///     .ttl(Duration::from_secs(60))
///     .build()
///     .unwrap();
/// // compile error: cannot bind &mut u32 from cache_get_or_set_with which returns &u32
/// let _: &mut u32 = cache.cache_get_or_set_with(1, || 2);
/// ```
#[cfg_attr(docsrs, doc(cfg(feature = "time_stores")))]
pub struct TtlSortedCache<K, V, S = DefaultHashBuilder> {
    // a minimum instant to compare ranges against since
    // all keys must logically expire after the creation
    // of the cache
    min_instant: Instant,

    // k/v where entry contains corresponds to an ordered value in `keys`
    map: HashMap<K, Entry<K, V>, S>,

    // ordered in ascending expiration `Instant`s
    // to support retaining/evicting without full traversal
    keys: BTreeSet<Stamped<K>>,

    pub(super) ttl: Duration,
    pub(super) size_limit: Option<usize>,
    // Preallocation hint captured at build so `cache_reset` can shrink back to
    // it instead of to zero, matching `TtlCache` (CORE-8).
    pub(super) initial_capacity: Option<usize>,
    pub(super) hits: StripedCounter,
    pub(super) misses: StripedCounter,
    pub(super) evictions: AtomicU64,
    pub(super) on_evict: Option<super::OnEvict<K, V>>,
}

impl<K, V, S> std::fmt::Debug for TtlSortedCache<K, V, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TtlSortedCache")
            .field("ttl", &self.ttl)
            .field("size_limit", &self.size_limit)
            .field("hits", &self.hits.load())
            .field("misses", &self.misses.load())
            .field("evictions", &self.evictions.load(AtomicOrdering::Relaxed))
            .field("on_evict", &self.on_evict.as_ref().map(|_| "on_evict"))
            .finish()
    }
}

impl<K, V, S> Clone for TtlSortedCache<K, V, S>
where
    K: Clone + Hash + Eq + Ord,
    V: Clone,
    S: Clone,
{
    fn clone(&self) -> Self {
        Self {
            min_instant: self.min_instant,
            map: self.map.clone(),
            keys: self.keys.clone(),
            ttl: self.ttl,
            size_limit: self.size_limit,
            initial_capacity: self.initial_capacity,
            hits: self.hits.snapshot(),
            misses: self.misses.snapshot(),
            evictions: AtomicU64::new(self.evictions.load(AtomicOrdering::Relaxed)),
            on_evict: self.on_evict.clone(),
        }
    }
}

/// Builder for [`TtlSortedCache`].
#[cfg_attr(docsrs, doc(cfg(feature = "time_stores")))]
pub struct TtlSortedCacheBuilder<K, V, S = DefaultHashBuilder> {
    size: Option<usize>,
    capacity: Option<usize>,
    ttl: Option<Duration>,
    on_evict: Option<super::OnEvict<K, V>>,
    hasher: S,
}

impl<K, V> Default for TtlSortedCacheBuilder<K, V, DefaultHashBuilder> {
    fn default() -> Self {
        Self {
            size: None,
            capacity: None,
            ttl: None,
            on_evict: None,
            hasher: super::new_default_hash_builder(),
        }
    }
}

impl<K, V> TtlSortedCacheBuilder<K, V> {
    /// Create a builder with default settings. Equivalent to [`TtlSortedCache::builder`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl<K, V, S> TtlSortedCacheBuilder<K, V, S> {
    /// Set the maximum number of entries (eviction bound). When the cache exceeds this
    /// limit, the next-to-expire entries are evicted until it is within bounds. Unlike
    /// [`initial_capacity`](Self::initial_capacity), this is a hard cap on entry count, not a
    /// preallocation hint.
    #[doc(alias = "size")]
    #[doc(alias = "capacity")]
    #[must_use]
    pub fn max_size(mut self, max_size: usize) -> Self {
        self.size = Some(max_size);
        self
    }

    /// Pre-allocate capacity for the backing store. This is a *preallocation hint* only —
    /// it does **not** bound the cache. Use [`max_size`](Self::max_size) to set the eviction
    /// bound. Reserves room for at least `capacity` entries in the backing map (the exact
    /// amount may be rounded up by the allocator), matching the preallocation semantics of
    /// the pre-2.0 `with_ttl_and_capacity` constructor.
    ///
    /// When set, this takes precedence over the preallocation implied by
    /// [`max_size`](Self::max_size): the backing map reserves for `capacity` entries rather
    /// than `max_size + 1`. This lets you cap entries at a large `max_size` while starting
    /// with a small allocation that grows on demand. Passing `capacity` larger than
    /// `max_size` is valid — the map simply starts larger; `max_size` still bounds the entry
    /// count. Only the backing map is pre-allocated; the `BTreeSet` TTL index is not.
    ///
    /// Note that [`set_max_size`](TtlSortedCache::set_max_size) on a live cache may re-grow
    /// the backing map to `max_size + 1`, overriding a smaller `initial_capacity` set here.
    #[must_use]
    pub fn initial_capacity(mut self, capacity: usize) -> Self {
        self.capacity = Some(capacity);
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

    /// Set a callback invoked when an entry is evicted. Fires for:
    /// - Size-limit evictions during insert (capacity-based, oldest-TTL-first).
    /// - TTL-expiry sweeps via [`evict`](TtlSortedCache::evict) and [`retain_latest`](TtlSortedCache::retain_latest).
    /// - Lazy expiry removal during [`cache_get`](crate::Cached::cache_get) / [`cache_get_mut`](crate::Cached::cache_get_mut).
    /// - Explicit [`cache_remove`](crate::Cached::cache_remove), including when the removed entry was already expired.
    ///
    /// Does **not** fire on [`cache_clear`](crate::Cached::cache_clear) / [`cache_reset`](crate::Cached::cache_reset).
    /// Use [`cache_clear_with_on_evict`](TtlSortedCache::cache_clear_with_on_evict)
    /// instead of [`cache_clear`](crate::Cached::cache_clear) to opt into callback
    /// firing and eviction counter increments when clearing all entries.
    #[must_use]
    pub fn on_evict(mut self, on_evict: impl Fn(&K, &V) + Send + Sync + 'static) -> Self {
        self.on_evict = Some(Arc::new(on_evict));
        self
    }

    /// Switch to a custom hash builder `S2`, returning a builder parameterized on `S2`.
    ///
    /// The hasher is used to hash keys in the internal `HashMap`. Calling this method
    /// changes the builder's type parameter so `build()` returns a `TtlSortedCache<K, V, S2>`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use cached::{Cached, stores::TtlSortedCache};
    /// use cached::time::Duration;
    /// use std::collections::hash_map::RandomState;
    ///
    /// let mut cache = TtlSortedCache::<u32, u32>::builder()
    ///     .ttl_secs(60)
    ///     .hasher(RandomState::new())
    ///     .build()
    ///     .unwrap();
    /// cache.cache_set(1, 100);
    /// assert_eq!(cache.cache_get(&1), Some(&100));
    /// ```
    #[doc(alias = "with_hasher")]
    #[must_use]
    pub fn hasher<S2: BuildHasher>(self, hasher: S2) -> TtlSortedCacheBuilder<K, V, S2> {
        TtlSortedCacheBuilder {
            size: self.size,
            capacity: self.capacity,
            ttl: self.ttl,
            on_evict: self.on_evict,
            hasher,
        }
    }

    /// Build the cache.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError`](super::BuildError) if `ttl` is not set or is zero, or if `size` is `0`.
    pub fn build(self) -> Result<TtlSortedCache<K, V, S>, super::BuildError>
    where
        K: Hash + Eq + Ord + Clone,
        S: BuildHasher,
    {
        let ttl = self.ttl.ok_or(super::BuildError::MissingRequired("ttl"))?;
        super::validate_ttl(ttl)?;
        if self.size == Some(0) {
            return Err(super::BuildError::InvalidValue {
                field: "max_size",
                reason: "must be greater than zero",
            });
        }
        let mut cache = TtlSortedCache {
            min_instant: Instant::now(),
            map: HashMap::with_hasher(self.hasher),
            keys: BTreeSet::new(),
            ttl,
            size_limit: self.size,
            initial_capacity: None,
            hits: StripedCounter::new(),
            misses: StripedCounter::new(),
            evictions: AtomicU64::new(0),
            on_evict: self.on_evict,
        };
        // Decide the single preallocation amount once all options are known.
        // An explicit `capacity` is the preallocation hint and takes precedence,
        // reserving for `capacity` and matching the old `with_ttl_and_capacity`.
        // Otherwise fall back to the previous internal behavior where a size limit
        // pre-reserved `size + 1` entries. We reserve only once: issuing the
        // `size + 1` reservation first would defeat a smaller explicit `capacity`,
        // since `HashMap::reserve` does not reduce an existing allocation.
        // A fallible `try_reserve` so an oversized `max_size`/`initial_capacity`
        // returns `Err(BuildError)` instead of aborting on capacity overflow,
        // matching `LruCache`/`LruTtlCache` (CORE-1).
        let (preallocate, field) = match self.capacity {
            Some(cap) => (Some(cap), "initial_capacity"),
            None => (self.size.map(|size| size.saturating_add(1)), "max_size"),
        };
        if let Some(amount) = preallocate {
            cache
                .map
                .try_reserve(amount)
                .map_err(|_| super::BuildError::InvalidValue {
                    field,
                    reason: "allocation failed",
                })?;
            cache.initial_capacity = Some(amount);
        }
        Ok(cache)
    }
}

impl<K: Hash + Eq + Ord + Clone, V> TtlSortedCache<K, V> {
    /// Construct a ready-to-use [`TtlSortedCache`] with the given `ttl` and no size bound.
    ///
    /// For optional settings (`max_size`, `capacity`, `on_evict`) use
    /// [`builder`](Self::builder).
    ///
    /// # Panics
    ///
    /// Panics if `ttl` is zero. Use [`builder`](Self::builder) with
    /// [`build`](TtlSortedCacheBuilder::build) to handle a zero TTL without panicking.
    #[must_use]
    pub fn new(ttl: Duration) -> Self {
        Self::builder()
            .ttl(ttl)
            .build()
            .expect("TtlSortedCache::new requires a non-zero ttl")
    }

    /// Return a builder for constructing a [`TtlSortedCache`].
    #[must_use]
    pub fn builder() -> TtlSortedCacheBuilder<K, V> {
        TtlSortedCacheBuilder::default()
    }
}

impl<K: Hash + Eq + Ord + Clone, V, S: BuildHasher> TtlSortedCache<K, V, S> {
    /// Set the maximum number of entries. When reached, the next entries to expire are evicted.
    /// Returns the previous value if one was set.
    ///
    /// If the new bound is smaller than the current entry count, entries are evicted immediately
    /// (in expiry order, next-to-expire first) so the cache is within the new bound on return,
    /// firing `on_evict` and counting each eviction. This matches
    /// [`LruCache::set_max_size`](super::LruCache::set_max_size), which also evicts down to the
    /// new bound eagerly rather than deferring to the next insert.
    ///
    /// The backing map grows on demand as entries are inserted (it is not pre-reserved here),
    /// matching [`LruCache::set_max_size`](super::LruCache::set_max_size); so this cannot abort
    /// on a capacity-overflowing `max_size`.
    ///
    /// # Panics
    ///
    /// Panics if `max_size` is 0. Use [`TtlSortedCache::try_set_max_size`] to handle invalid
    /// sizes without panicking.
    ///
    /// # See also
    ///
    /// [`LruCache::set_max_size`](super::LruCache::set_max_size) and
    /// [`LruTtlCache::set_max_size`](super::LruTtlCache::set_max_size) are parallel methods
    /// on the other LRU-family stores. Note that this method returns `Option<usize>` (the
    /// previous bound, which is optional) rather than `usize`, because `TtlSortedCache` does
    /// not require a size bound at construction. All stores also provide a fallible
    /// `try_set_max_size` counterpart.
    pub fn set_max_size(&mut self, max_size: usize) -> Option<usize> {
        assert!(max_size > 0, "max_size must be greater than zero");
        let prev = self.size_limit;
        self.size_limit = Some(max_size);
        // Evict down to the new bound immediately rather than waiting for the next insert, so a
        // shrink takes effect on return (matching `LruCache::set_max_size`). `retain_latest`
        // drops the next-to-expire entries first, firing `on_evict` and counting each eviction.
        if self.map.len() > max_size {
            let _ = self.retain_latest(max_size, false);
        }
        prev
    }

    /// Set a non-zero maximum number of entries. When reached, the next entries to expire are evicted.
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

    /// Returns the maximum number of entries this cache will hold before evicting,
    /// or `None` if no size bound is configured.
    ///
    /// This is the bound set via [`TtlSortedCacheBuilder::max_size`] /
    /// [`set_max_size`](Self::set_max_size), not the current number of entries — use
    /// [`cache_size`](crate::Cached::cache_size) for that. Unlike
    /// [`LruCache::capacity`](crate::LruCache::capacity) (which returns `usize`),
    /// this returns `Option<usize>` because the bound is optional for this store.
    #[doc(alias = "size")]
    #[doc(alias = "max_size")]
    #[must_use]
    pub fn capacity(&self) -> Option<usize> {
        self.size_limit
    }

    /// Increase backing stores with enough capacity to store `more`
    pub fn reserve(&mut self, more: usize) {
        self.map.reserve(more);
    }

    /// Evict values that have expired.
    /// Returns number of dropped items.
    #[must_use]
    pub fn evict(&mut self) -> usize {
        self.evict_at(Instant::now())
    }

    /// [`evict`](Self::evict) against an explicit `cutoff`, so a caller that already sampled
    /// the clock (e.g. [`set_inner`](Self::set_inner) on an evicting insert) does not pay for a
    /// second `Instant::now()`.
    ///
    /// The expired entries are exactly the front prefix of `self.keys`: the index is ordered by
    /// expiry and `None` (never-expires) sorts GREATEST (see [`Stamped`]'s `Ord`). Rather than
    /// counting a range and then popping the same prefix one entry at a time (two traversals,
    /// and an `O(log n)` rebalance per popped entry), the prefix is detached in one
    /// `split_off` and drained by value: the split is a single `O(log n)` descent and the
    /// drain is an in-order walk of a tree that is being consumed, so no rebalancing happens
    /// at all.
    ///
    /// The boundary is `expiry < cutoff` (strictly-less), exactly what the previous
    /// `range(Included(bound(min_instant))..Excluded(bound(cutoff)))` selected: the sentinel
    /// `Stamped::bound(cutoff)` carries `key: None`, which sorts below every real key at the
    /// same expiry, so a real entry expiring exactly at `cutoff` stays on the live side under
    /// `split_off` just as it was excluded from the old range. That matches `is_expired_at`'s
    /// at-or-after boundary in practice: `cutoff` is sampled by the caller, so no stored expiry
    /// (computed at an earlier insert) ever equals it, and any entry that did tie is caught by
    /// the next `is_expired` get. The old `min_instant` lower bound is likewise unnecessary —
    /// every stored expiry is `>= min_instant` by construction (it is `insert_time + ttl` and
    /// the cache was built before any insert) — so dropping it saves a tree descent.
    ///
    /// Returns the number of index entries dropped (matching the old `pop_first` count), while
    /// `evictions` counts only the entries that were actually present in the map.
    fn evict_at(&mut self, cutoff: Instant) -> usize {
        // Detach `[.., cutoff)` in one operation: `split_off` leaves everything BELOW the
        // sentinel in `self.keys` and returns the rest, so swap the two halves back.
        let live = self.keys.split_off(&Stamped::bound(cutoff));
        let expired = std::mem::replace(&mut self.keys, live);
        let count = expired.len();
        if count == 0 {
            return 0;
        }

        if self.on_evict.is_none() {
            // Drain the map FIRST, moving every removed `Entry` into `removed`, and let the
            // values/keys drop only after the whole prefix has left `self.map` — exactly as
            // the callback branch below does. A per-iteration drop (dropping each `Entry`
            // inside the loop) would let a panicking `Drop` for `V` or `K` unwind with the
            // remaining stamps already detached from `self.keys` but their entries still in
            // `self.map`: orphaned rows, invisible to a later sweep yet still counted by
            // `len`. Collecting keeps `self.map` and `self.keys` in lockstep across a panic.
            let mut removed = Vec::with_capacity(count);
            for stamped in expired {
                // Invariant: `None` keys are only used as artificial range sentinels
                // in `evict()`/`retain_latest()` and are never inserted into `self.keys`.
                let key = stamped
                    .key
                    .expect("evicting: only artificial bounds are none");
                if let Some(entry) = self.map.remove(key.0.as_ref()) {
                    removed.push((key, entry));
                }
            }
            // Count before the drop: a `Drop` panic while `removed` unwinds must not leave
            // the counter short of what was actually pulled from the map.
            self.evictions
                .fetch_add(removed.len() as u64, AtomicOrdering::Relaxed);
            return count;
        }

        // With a callback configured, drain both structures FIRST and fire `on_evict` only
        // once every removal is done — as `retain` does. Firing mid-drain would let a
        // panicking callback unwind with the remaining stamps already detached from
        // `self.keys` but their entries still in `self.map`, orphaning them: invisible to a
        // later sweep yet still counted by `len`.
        let mut removed = Vec::with_capacity(count);
        for stamped in expired {
            let key = stamped
                .key
                .expect("evicting: only artificial bounds are none");
            if let Some(entry) = self.map.remove(key.0.as_ref()) {
                removed.push((key, entry));
            }
        }
        self.evictions
            .fetch_add(removed.len() as u64, AtomicOrdering::Relaxed);
        if let Some(on_evict) = &self.on_evict {
            for (key, entry) in &removed {
                on_evict(key.0.as_ref(), &entry.value);
            }
        }
        count
    }

    /// Retain only entries that are unexpired and satisfy `keep`.
    ///
    /// Removes every entry that is already TTL-expired **or** for which `keep`
    /// returns `false` — expired entries are removed without consulting `keep`.
    /// `on_evict` is called and the eviction counter incremented for each removed
    /// entry. Entries stored with no expiry (a zero or overflowing TTL, see
    /// [`set_with`](Self::set_with)) never expire, so they are removed only when
    /// `keep` returns `false`. Returns `()`; use [`evict`](Self::evict) or
    /// [`retain_latest`](Self::retain_latest) when a dropped count is needed.
    ///
    /// Not to be confused with [`retain_latest`](Self::retain_latest), which is a
    /// *size trim*: it drops the next-to-expire entries until at most `count` remain
    /// (optionally also sweeping expired ones) and returns how many it dropped. This
    /// method is a *predicate filter plus an expiry sweep*: it consults `keep` for
    /// every live entry, ignores `size_limit`, and does not reorder the expiry index.
    ///
    /// This matches [`TtlCache::retain`](crate::TtlCache::retain) and
    /// [`LruTtlCache::retain`](crate::LruTtlCache::retain); the plain
    /// [`LruCache::retain`](crate::LruCache::retain) has no expiry dimension and
    /// removes solely on the predicate.
    pub fn retain<F: FnMut(&K, &V) -> bool>(&mut self, mut keep: F) {
        // Sample the clock once so every entry is judged against the same instant.
        let now = Instant::now();
        // Disjoint field borrows: `map.retain` takes `&mut self.map` while the closure
        // holds `&mut self.keys` plus shared borrows of the callback and counter.
        let keys = &mut self.keys;
        // Drain both structures first and fire `on_evict` only once the pass is over.
        // Firing mid-pass would let a panicking callback unwind between the index
        // removal and the map removal, leaving `keys` short of `map` permanently:
        // the orphaned entry would be invisible to `evict`/`retain_latest` (their
        // `pop_first` walk never reaches it) yet still counted by `len`.
        let removed: Vec<_> = self
            .map
            .extract_if(|key, entry| {
                if entry.is_expired_at(now) || !keep(key, &entry.value) {
                    // `as_stamped` rebuilds the exact `Stamped` that was inserted (same
                    // expiry, same `CacheArc` key), so this cannot leave a stale index
                    // entry that a later `pop_first` would miscount as a drop.
                    keys.remove(&entry.as_stamped());
                    true
                } else {
                    false
                }
            })
            .collect();
        self.evictions
            .fetch_add(removed.len() as u64, AtomicOrdering::Relaxed);
        if let Some(on_evict) = &self.on_evict {
            for (key, entry) in &removed {
                on_evict(key, &entry.value);
            }
        }
    }

    /// Retain only the latest `count` values, dropping the next values to expire.
    /// If `evict`, then also evict values that have expired.
    /// Returns number of dropped items.
    ///
    /// This is a *size trim*, not a predicate filter: entries are chosen purely by
    /// expiry order until at most `count` remain. Use [`retain`](Self::retain) to keep
    /// entries by a `FnMut(&K, &V) -> bool` predicate (which also sweeps expired entries
    /// regardless of the predicate, but ignores `size_limit` and returns `()`).
    pub fn retain_latest(&mut self, count: usize, evict: bool) -> usize {
        self.retain_latest_at(count, evict.then(Instant::now))
    }

    /// [`retain_latest`](Self::retain_latest) with the expiry sweep driven by an explicit
    /// cutoff: `Some(cutoff)` is the `evict = true` sweep, `None` disables it. Lets a caller
    /// that already sampled the clock reuse its own `now`.
    ///
    /// Like [`evict_at`](Self::evict_at) this walks the front of the expiry index instead of
    /// pre-counting a range. The old code took `max(retain_drop_count, expired_count)` and
    /// popped that many; since the expired entries are exactly a front prefix, popping while
    /// `dropped < retain_drop_count || (evict && front_is_expired)` removes the same set.
    fn retain_latest_at(&mut self, count: usize, cutoff: Option<Instant>) -> usize {
        let retain_drop_count = self.map.len().saturating_sub(count);
        if retain_drop_count == 0 {
            // No size trim to do: this is either a pure expiry sweep (where the old
            // `max(0, expired_count)` is just the sweep count) or a complete no-op that must
            // leave the index untouched.
            return match cutoff {
                Some(cutoff) => self.evict_at(cutoff),
                None => 0,
            };
        }

        let mut dropped = 0;
        while let Some(stamped) = self.keys.pop_first() {
            if dropped >= retain_drop_count {
                // Size trim satisfied; keep going only while the front is expired.
                let expired = match cutoff {
                    Some(cutoff) => matches!(stamped.expiry, Some(expiry) if expiry < cutoff),
                    None => false,
                };
                if !expired {
                    self.keys.insert(stamped);
                    break;
                }
            }
            // Invariant: same as evict() — None keys are sentinel-only.
            let key = stamped
                .key
                .expect("retaining: only artificial bounds are none");
            if let Some(entry) = self.map.remove(key.0.as_ref()) {
                self.evictions.fetch_add(1, AtomicOrdering::Relaxed);
                if let Some(on_evict) = &self.on_evict {
                    on_evict(key.0.as_ref(), &entry.value);
                }
            }
            dropped += 1;
        }
        dropped
    }

    /// Set k/v pair without running eviction logic, using the cache's default TTL.
    ///
    /// The entry is inserted first. If a `size_limit` was configured and the insertion
    /// pushes the map over it, the soonest-to-expire entry is trimmed to restore the
    /// bound; this size-limit enforcement runs on every `set`, independent of the
    /// `.evict()` opt-in. The `.set_with(..).evict()` opt-in controls only the separate
    /// expiry sweep (dropping entries already past their TTL), not size-limit
    /// enforcement; see [`set_with`](Self::set_with) for per-entry TTL overrides and the
    /// opt-in sweep.
    ///
    /// If computing the expiry instant overflows (a TTL on the order of hundreds of
    /// years), the entry is stored with no expiry (never expires), matching
    /// [`cache_set`](crate::Cached::cache_set) on the other TTL stores.
    pub fn set(&mut self, key: K, value: V) -> Option<V> {
        self.set_inner(key, value, None, false, false).0
    }

    /// Start building a `set` call with an optional per-entry TTL override and/or
    /// opt-in eviction, e.g. `cache.set_with(k, v).ttl(Duration::from_secs(5)).evict().set()`.
    ///
    /// The entry is inserted first. If a `size_limit` was specified and capacity is exceeded,
    /// the next-to-expire entry is dropped after insertion. The eviction callback fires after
    /// insertion, not before. The terminal [`.set()`](TtlSortedSetBuilder::set) returns any
    /// existing unexpired value that was replaced.
    ///
    /// If computing the expiry instant overflows (a TTL on the order of hundreds of
    /// years), the entry is stored with no expiry (never expires), matching
    /// [`cache_set`](crate::Cached::cache_set) on the other TTL stores.
    #[must_use = "set_with does nothing until .set() is called"]
    pub fn set_with(&mut self, key: K, value: V) -> TtlSortedSetBuilder<'_, K, V, S> {
        TtlSortedSetBuilder {
            cache: self,
            key,
            value,
            ttl: None,
            evict: false,
        }
    }

    /// Shared insertion routine for [`set_with`](Self::set_with) / [`set`](Self::set) and the
    /// `cache_get_or_set_with_mut` paths.
    ///
    /// When the effective TTL (explicit `ttl` arg or `self.ttl`) is zero, the entry is
    /// stored with `expiry = None` (never expires) rather than being given an immediate
    /// expiry. Zero TTL means "disable expiry" for new inserts, consistent with the other
    /// TTL stores. In the (practically unreachable) case where `now + ttl` exceeds
    /// `Instant`'s representable range — a TTL on the order of hundreds of years — the
    /// entry is likewise stored with `expiry = None`, matching the never-expires-on-overflow
    /// behavior of `TtlCache` / `LruTtlCache` and the sharded TTL stores.
    ///
    /// `skip_size_eviction` defers size-limit enforcement to the caller
    /// (`set_and_get_mut` must protect the just-inserted entry before evicting).
    ///
    /// Returns the displaced (unexpired) value plus the `Stamped` that was written to the
    /// expiry index. Handing the stamp back lets `set_and_get_mut` protect and re-find the
    /// entry it just inserted without cloning the key again or re-hashing it; the stamp holds
    /// the very same `CacheArc` handle the stored `Entry` does (an `Arc`, so no deep clone).
    fn set_inner(
        &mut self,
        key: K,
        value: V,
        ttl: Option<Duration>,
        evict: bool,
        skip_size_eviction: bool,
    ) -> (Option<V>, Stamped<K>) {
        let effective_ttl = ttl.unwrap_or(self.ttl);

        // Sample the clock ONCE for the whole operation: the new expiry, the displaced
        // entry's expiry check, and any eager sweep below are all judged against this
        // instant instead of taking two or three separate `Instant::now()` readings.
        let now = Instant::now();

        // A zero TTL means "never expires": store expiry = None. `checked_add`
        // returning `None` on overflow lands on the same never-expires representation.
        let expiry = if effective_ttl.is_zero() {
            None
        } else {
            now.checked_add(effective_ttl)
        };

        // `entry` rather than `insert`: on the occupied path the existing `Entry` already owns
        // a `CacheArc` for this key, so reusing it is a refcount bump instead of an allocation
        // plus a deep `K::clone`. Only the vacant path allocates, and it clones the key out of
        // `VacantEntry::key` (the caller's `key` is moved into the map by `insert`).
        let (arc_key, old) = match self.map.entry(key) {
            std::collections::hash_map::Entry::Occupied(mut occupied) => {
                let arc_key = occupied.get().key.clone();
                let old = occupied.insert(Entry {
                    expiry,
                    key: arc_key.clone(),
                    value,
                });
                (arc_key, Some(old))
            }
            std::collections::hash_map::Entry::Vacant(vacant) => {
                let arc_key = CacheArc::new(vacant.key().clone());
                vacant.insert(Entry {
                    expiry,
                    key: arc_key.clone(),
                    value,
                });
                (arc_key, None)
            }
        };

        let new_stamped = Stamped {
            expiry,
            key: Some(arc_key),
        };
        self.keys.insert(new_stamped.clone());
        // The displaced entry's stamp carries the same (equal) key, so it differs from the new
        // one exactly when the expiry changed — comparing the two instants avoids rebuilding
        // `old.as_stamped()` on the (common) unchanged-expiry path.
        if let Some(old) = &old
            && old.expiry != expiry
        {
            self.keys.remove(&old.as_stamped());
        }
        let old_value = match old {
            // A displaced expired value is filtered from the return (matching the get paths), so
            // it is dropped silently from the caller's view; fire `on_evict` and count an
            // eviction so cleanup and metrics stay consistent with the other removal paths.
            Some(entry) if entry.is_expired_at(now) => {
                if let Some(on_evict) = &self.on_evict {
                    on_evict(entry.key.0.as_ref(), &entry.value);
                }
                self.evictions.fetch_add(1, AtomicOrdering::Relaxed);
                None
            }
            Some(entry) => Some(entry.value),
            None => None,
        };

        // Size-limit eviction is skipped only when the caller explicitly requests it
        // (`skip_size_eviction`) — e.g. `set_and_get_mut` must guarantee the just-inserted
        // entry is still present to return `&mut V` safely, regardless of the entry's TTL.
        // The sweeps reuse `now` rather than re-reading the clock.
        if !skip_size_eviction {
            if let Some(size_limit) = self.size_limit {
                if self.map.len() > size_limit {
                    self.retain_latest_at(size_limit, evict.then_some(now));
                }
            } else if evict {
                let _ = self.evict_at(now);
            }
        }

        (old_value, new_stamped)
    }

    /// Insert `key`/`value` and return a mutable reference to the stored value.
    ///
    /// When a `size_limit` is configured the just-inserted entry is
    /// protected from eviction: other entries are evicted in TTL order to restore
    /// capacity. Used by the `cache_get_or_set_with_mut` family.
    fn set_and_get_mut(&mut self, key: K, value: V) -> &mut V {
        // `skip_size_eviction = true` defers size enforcement to the block below,
        // where we can protect the just-inserted entry. `set_inner` hands back the exact
        // `Stamped` it indexed, so the key is moved in (no extra `K::clone`) and the
        // protect/re-find steps below reuse that stamp instead of re-deriving it from a
        // second map lookup.
        let (_, protected) = self.set_inner(key, value, None, false, true);

        if let Some(size_limit) = self.size_limit
            && self.map.len() > size_limit
        {
            // Temporarily unlink the just-inserted entry from the expiry index so
            // `retain_latest` cannot select it for eviction. Other entries are
            // dropped in TTL order until the map is back within `size_limit`.
            // The stamp is restored afterward so the index stays consistent.
            self.keys.remove(&protected);
            self.retain_latest(size_limit, false);
            self.keys.insert(protected.clone());
        }

        // The stamp shares the stored entry's key `Arc`, so this borrows the key rather than
        // cloning it. The `&mut V` is tied to `&mut self`, not to `protected`.
        let key: &K = protected
            .key
            .as_ref()
            .expect("set_and_get_mut: set_inner always stamps a real key")
            .0
            .as_ref();
        &mut self
            .map
            .get_mut(key)
            .expect("set_and_get_mut: the protected eviction path guarantees the entry is present")
            .value
    }

    fn remove_expired_entry<Q>(&mut self, key: &Q)
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        if let Some(entry) = self.map.remove(key) {
            self.keys.remove(&entry.as_stamped());
            if let Some(on_evict) = &self.on_evict {
                on_evict(entry.key.0.as_ref(), &entry.value);
            }
            self.evictions.fetch_add(1, AtomicOrdering::Relaxed);
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
        let entries: Vec<(K, Entry<K, V>)> = self.map.drain().collect();
        self.keys.clear();
        let count = entries.len() as u64;
        if count > 0 {
            self.evictions.fetch_add(count, AtomicOrdering::Relaxed);
        }
        if let Some(on_evict) = &self.on_evict {
            for (_k, entry) in &entries {
                on_evict(entry.key.0.as_ref(), &entry.value);
            }
        }
    }
}

/// Builder returned by [`TtlSortedCache::set_with`] for chaining a per-entry TTL override
/// and/or opt-in eviction before performing the insertion.
///
/// Nothing is inserted until the terminal [`.set()`](Self::set) is called — the `#[must_use]`
/// attribute flags a builder that is constructed and dropped without ever calling it.
#[cfg_attr(docsrs, doc(cfg(feature = "time_stores")))]
#[must_use = "set_with does nothing until .set() is called"]
pub struct TtlSortedSetBuilder<'a, K, V, S = DefaultHashBuilder> {
    cache: &'a mut TtlSortedCache<K, V, S>,
    key: K,
    value: V,
    ttl: Option<Duration>,
    evict: bool,
}

impl<'a, K: Hash + Eq + Ord + Clone, V, S: BuildHasher> TtlSortedSetBuilder<'a, K, V, S> {
    /// Override the store's default TTL for this entry only.
    ///
    /// If computing the expiry instant overflows (a TTL on the order of hundreds of
    /// years), the entry is stored with no expiry (never expires), matching
    /// [`cache_set`](crate::Cached::cache_set) on the other TTL stores. A `Duration::ZERO`
    /// TTL also means "never expires" for this entry, matching the store's zero-TTL
    /// convention.
    pub fn ttl(mut self, ttl: Duration) -> Self {
        self.ttl = Some(ttl);
        self
    }

    /// Opt into running eviction logic after this insertion.
    ///
    /// The entry is inserted first. If a `size_limit` was specified and capacity is
    /// exceeded, the next-to-expire entry is dropped after insertion, firing `on_evict`
    /// and counting an eviction.
    pub fn evict(mut self) -> Self {
        self.evict = true;
        self
    }

    /// Perform the insertion. Returns any existing unexpired value that was replaced,
    /// or `None` if the key was absent or the replaced entry had already expired.
    pub fn set(self) -> Option<V> {
        self.cache
            .set_inner(self.key, self.value, self.ttl, self.evict, false)
            .0
    }
}

impl<K: Hash + Eq + Ord + Clone, V, S: BuildHasher> Cached<K, V> for TtlSortedCache<K, V, S> {
    type Error = std::convert::Infallible;

    fn cache_get<Q>(&mut self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        // Two lookups on the hit path: the first checks expiry (releasing the borrow via
        // `.map`), the second returns the reference. A single-lookup approach is not possible
        // SAFELY in stable Rust because returning `&'1 V` from inside an `if let` block ties
        // the borrow to lifetime `'1`, which prevents `remove_entry` (a mutable borrow) even on
        // the non-returning path. Polonius (nightly) would fix this. It IS possible with
        // `unsafe`: `TtlCache::cache_get` (see `src/stores/ttl.rs`, the `&entry.value as
        // *const V` reborrow) collapses this to one lookup behind a documented SAFETY comment.
        let is_expired = match self.map.get(key) {
            None => {
                self.misses.increment();
                return None;
            }
            Some(entry) => entry.is_expired(),
        };

        if is_expired {
            self.misses.increment();
            self.remove_expired_entry(key);
            return None;
        }

        self.hits.increment();
        self.map.get(key).map(|e| &e.value)
    }

    fn cache_get_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let is_expired = match self.map.get(key) {
            None => {
                self.misses.increment();
                return None;
            }
            Some(entry) => entry.is_expired(),
        };

        if is_expired {
            self.misses.increment();
            self.remove_expired_entry(key);
            return None;
        }

        self.hits.increment();
        self.map.get_mut(key).map(|e| &mut e.value)
    }

    fn cache_set(&mut self, key: K, value: V) -> Option<V> {
        self.set_inner(key, value, None, false, false).0
    }

    fn cache_get_or_set_with_mut<F: FnOnce() -> V>(&mut self, key: K, f: F) -> &mut V {
        // Check liveness WITHOUT removing the entry, then run the factory, then replace on
        // success. This mirrors `TtlCache` / `LruTtlCache`: a factory panic leaves the stale
        // entry in place instead of dropping it (and does not fire `on_evict` prematurely).
        // The borrow from `map.get` ends with the `matches!`, so the counters/`get_mut` below
        // are free to borrow `self` again (the Polonius limitation, see `cache_get`).
        let live = matches!(self.map.get(&key), Some(entry) if !entry.is_expired());
        if live {
            self.hits.increment();
            return self
                .map
                .get_mut(&key)
                .map(|entry| &mut entry.value)
                // Invariant: the liveness check above confirmed the entry exists and is not
                // expired. No other code path removes it between the check and this get_mut.
                .expect("cache entry vanished");
        }
        self.misses.increment();
        // Miss or expired entry: build the value first. `set_and_get_mut` inserts it and, when
        // it displaces an expired entry, fires `on_evict` and counts one eviction (insert_inner).
        // It never drops the value (it saturates an unrepresentable TTL instead of erroring), so
        // this path is panic-free once `f()` has produced a value.
        self.set_and_get_mut(key, f())
    }

    fn cache_try_get_or_set_with_mut<F: FnOnce() -> Result<V, E>, E>(
        &mut self,
        key: K,
        f: F,
    ) -> Result<&mut V, E> {
        // Same structure as `cache_get_or_set_with_mut`: check liveness without removing, run
        // the factory, replace only on `Ok`. On `Err` the (expired or absent) entry is left
        // exactly as it was and `on_evict` does not fire, matching `TtlCache` / `LruTtlCache`.
        let live = matches!(self.map.get(&key), Some(entry) if !entry.is_expired());
        if live {
            self.hits.increment();
            return Ok(self
                .map
                .get_mut(&key)
                .map(|entry| &mut entry.value)
                // Invariant: same as cache_get_or_set_with_mut above.
                .expect("cache entry vanished"));
        }
        self.misses.increment();
        let value = f()?;
        // `set_and_get_mut` never drops the value, so this path is panic-free.
        Ok(self.set_and_get_mut(key, value))
    }

    fn cache_remove<Q>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        match self.map.remove(key) {
            None => None,
            Some(removed) => {
                let expired = removed.is_expired();
                self.keys.remove(&removed.as_stamped());
                // The stored key `Arc` derefs to `&K` directly: no clone is needed to hand the
                // callback a reference (and the clone previously ran even without a callback).
                if let Some(on_evict) = &self.on_evict {
                    on_evict(removed.key.0.as_ref(), &removed.value);
                }
                self.evictions.fetch_add(1, AtomicOrdering::Relaxed);
                if expired { None } else { Some(removed.value) }
            }
        }
    }

    fn cache_remove_entry<Q>(&mut self, key: &Q) -> Option<(K, V)>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        match self.map.remove(key) {
            None => None,
            Some(removed) => {
                self.keys.remove(&removed.as_stamped());
                let stored_k = (*removed.key.0).clone();
                if let Some(on_evict) = &self.on_evict {
                    on_evict(&stored_k, &removed.value);
                }
                self.evictions.fetch_add(1, AtomicOrdering::Relaxed);
                Some((stored_k, removed.value))
            }
        }
    }

    fn cache_clear(&mut self) {
        // Inline rather than delegate to a `self.clear()` shim — the `Cached`
        // short alias `clear` defaults to `cache_clear`, so going through it
        // would be circular.
        self.map.clear();
        self.keys.clear();
    }

    fn cache_reset(&mut self) {
        // Entries are dropped in-place; `on_evict` is NOT called for cleared entries.
        // Use clear + shrink_to to avoid needing S: Clone to rebuild the HashMap.
        // Shrink back to the build-time preallocation hint, not to zero, so the
        // configured capacity survives a reset (CORE-8), matching `TtlCache`.
        self.map.clear();
        self.map.shrink_to(self.initial_capacity.unwrap_or(0));
        self.keys = BTreeSet::new();
        self.min_instant = Instant::now();
        self.cache_reset_metrics();
    }

    fn cache_reset_metrics(&mut self) {
        self.misses.reset();
        self.hits.reset();
        self.evictions.store(0, AtomicOrdering::Relaxed);
    }

    /// Reports raw entry count without sweeping; the count may include
    /// expired entries. Run [`evict`](TtlSortedCache::evict) or
    /// [`retain_latest`](TtlSortedCache::retain_latest) first for an accurate
    /// post-sweep count.
    fn cache_size(&self) -> usize {
        self.map.len()
    }

    fn cache_hits(&self) -> Option<u64> {
        Some(self.hits.load())
    }

    fn cache_misses(&self) -> Option<u64> {
        Some(self.misses.load())
    }

    fn cache_evictions(&self) -> Option<u64> {
        Some(self.evictions.load(AtomicOrdering::Relaxed))
    }

    fn cache_capacity(&self) -> Option<usize> {
        self.size_limit
    }

    /// Returns `true` if the key is present and its entry has not expired.
    ///
    /// Uses `cache_peek` internally: no hit/miss counters are updated and no
    /// recency or TTL refresh occurs. Expired entries report `false`.
    fn cache_contains<Q>(&mut self, k: &Q) -> bool
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        crate::CachedPeek::cache_peek(self, k).is_some()
    }
}

impl<K: Hash + Eq + Ord, V, S: BuildHasher> CachedIter<K, V> for TtlSortedCache<K, V, S> {
    fn iter<'a>(&'a self) -> impl Iterator<Item = (&'a K, &'a V)> + 'a
    where
        K: 'a,
        V: 'a,
    {
        // The clock is read per item rather than hoisted into a construction-time snapshot:
        // this iterator is lazy and may be held across a long consumer loop, where a stale
        // snapshot would report entries that have since expired as live.
        self.map.iter().filter_map(|(k, entry)| {
            if entry.is_expired() {
                None
            } else {
                Some((k, &entry.value))
            }
        })
    }
}

impl<K: Hash + Eq + Ord, V, S: BuildHasher> CacheTtl for TtlSortedCache<K, V, S> {
    /// Returns the currently configured TTL, or `None` when expiry is disabled.
    ///
    /// When `ttl` is `Duration::ZERO`, expiry is disabled: entries inserted while zero is
    /// set never expire (they are stored with `expiry = None`). This resolves a zero TTL to
    /// `None`, consistent with `TtlCache` and `LruTtlCache`, rather than reporting the raw
    /// `Some(Duration::ZERO)`.
    fn ttl(&self) -> Option<Duration> {
        // A zero TTL means expiry is disabled.
        if self.ttl.is_zero() {
            None
        } else {
            Some(self.ttl)
        }
    }
    /// Set the global TTL for future inserts, returning the previous value (or `None` if
    /// expiry was already disabled).
    ///
    /// A zero `Duration` disables expiry for **future** inserts: entries inserted while the TTL
    /// is zero are stored with `expiry = None` and never expire. Pre-existing entries keep their
    /// original expiry and still expire on schedule. This is consistent with the other TTL stores
    /// (`TtlCache`, `LruTtlCache`). To restore expiry, call `set_ttl` with a non-zero duration.
    fn set_ttl(&mut self, ttl: Duration) -> Option<Duration> {
        let old = self.ttl;
        self.ttl = ttl;
        if old.is_zero() { None } else { Some(old) }
    }
    /// Disable expiry for future inserts by setting the TTL to `Duration::ZERO`.
    ///
    /// Equivalent to `set_ttl(Duration::ZERO)`: entries inserted after this call never expire.
    /// Pre-existing entries keep their original expiry. Returns the previous TTL, or `None` if
    /// expiry was already disabled, matching `TtlCache` / `LruTtlCache`.
    fn unset_ttl(&mut self) -> Option<Duration> {
        let old = self.ttl;
        self.ttl = Duration::ZERO;
        if old.is_zero() { None } else { Some(old) }
    }
    /// `TtlSortedCache` does not refresh entries on hit; always returns `false`.
    fn refresh_on_hit(&self) -> bool {
        false
    }
    /// `TtlSortedCache` does not support refresh-on-hit; this is a no-op and always returns `false`.
    fn set_refresh_on_hit(&mut self, _refresh: bool) -> bool {
        false
    }
}

impl<K: Hash + Eq + Ord, V, S: BuildHasher> CachedPeek<K, V> for TtlSortedCache<K, V, S> {
    fn cache_peek<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.map.get(key).and_then(|entry| {
            if entry.is_expired() {
                None
            } else {
                Some(&entry.value)
            }
        })
    }
}

impl<K: Hash + Eq + Ord, V, S: BuildHasher> CachedRead<K, V> for TtlSortedCache<K, V, S> {
    fn cache_get_read<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        if let Some(value) = self.cache_peek(key) {
            self.hits.increment();
            Some(value)
        } else {
            self.misses.increment();
            None
        }
    }
}

impl<K: Hash + Eq + Ord + Clone, V: Clone, S: BuildHasher + Clone> CloneCached<K, V>
    for TtlSortedCache<K, V, S>
{
    fn cache_get_with_expiry_status<Q>(&mut self, k: &Q) -> (Option<V>, bool)
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        match self.map.get(k) {
            None => {
                self.misses.increment();
                (None, false)
            }
            Some(entry) if entry.is_expired() => {
                self.misses.increment();
                (Some(entry.value.clone()), true)
            }
            Some(entry) => {
                self.hits.increment();
                (Some(entry.value.clone()), false)
            }
        }
    }

    /// Peek at the entry (including expired entries) without any read side effects.
    ///
    /// Returns `(Some(v), true)` for an expired entry, `(Some(v), false)` for a live
    /// entry, and `(None, false)` when the key is absent. Does not update hit/miss
    /// counters or renew the TTL.
    fn cache_peek_with_expiry_status<Q>(&self, k: &Q) -> (Option<V>, bool)
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
        V: Clone,
    {
        match self.map.get(k) {
            None => (None, false),
            Some(entry) if entry.is_expired() => (Some(entry.value.clone()), true),
            Some(entry) => (Some(entry.value.clone()), false),
        }
    }
}

#[cfg(feature = "async_core")]
#[cfg_attr(docsrs, doc(cfg(feature = "async_core")))]
impl<K, V, S> CachedGetOrSetAsync<K, V> for TtlSortedCache<K, V, S>
where
    K: Hash + Eq + Ord + Clone + Send + Sync,
    V: Send,
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
            // Same shape as the sync `cache_get_or_set_with_mut`: check liveness in place
            // (one lookup) instead of going through `cache_get`, which costs two lookups of
            // its own before this method's `get_mut` — three on an async hit. As in the sync
            // version, an expired entry is left in place for `set_and_get_mut` to displace
            // (which fires `on_evict` and counts the eviction) rather than being swept before
            // the future runs, so a dropped/panicking future leaves the stale entry alone.
            let live = matches!(self.map.get(&k), Some(entry) if !entry.is_expired());
            if live {
                self.hits.increment();
                return self
                    .map
                    .get_mut(&k)
                    .map(|entry| &mut entry.value)
                    // Invariant: the liveness check above confirmed the entry is present and
                    // unexpired, and nothing removes it in between.
                    .expect("cache entry vanished");
            }
            self.misses.increment();
            // `set_and_get_mut` never drops the value, so this path is panic-free.
            let value = f().await;
            self.set_and_get_mut(k, value)
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
            // Same shape as `async_cache_get_or_set_with_mut` / the sync fallible sibling:
            // check liveness in place (one lookup) instead of going through `cache_get`, which
            // would sweep an expired entry (and fire `on_evict`) before the factory is even
            // polled. An expired entry is left in place for `set_and_get_mut` to displace on
            // `Ok` (which fires `on_evict` and counts the eviction) rather than being swept
            // up front, so a factory `Err` or a dropped/cancelled future leaves the stale entry
            // alone.
            let live = matches!(self.map.get(&k), Some(entry) if !entry.is_expired());
            if live {
                self.hits.increment();
                return Ok(self
                    .map
                    .get_mut(&k)
                    .map(|entry| &mut entry.value)
                    // Invariant: the liveness check above confirmed the entry is present and
                    // unexpired, and nothing removes it in between.
                    .expect("cache entry vanished"));
            }
            self.misses.increment();
            let value = f().await?;
            // `set_and_get_mut` never drops the value, so this path is panic-free.
            Ok(self.set_and_get_mut(k, value))
        }
    }
}

impl<K: std::hash::Hash + Eq + Ord + Clone, V, S: BuildHasher> CacheEvict
    for TtlSortedCache<K, V, S>
{
    fn evict(&mut self) -> usize {
        TtlSortedCache::evict(self)
    }
}

#[cfg(test)]
mod test {
    use crate::stores::TtlSortedCache;
    use crate::time::Duration;
    use crate::{CacheTtl, Cached, CachedExt, CachedRead};
    use std::cmp::Ordering as CmpOrdering;
    use std::hash::{Hash, Hasher};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn cache_set_over_expired_returns_none_fires_on_evict_and_counts() {
        let fired = Arc::new(AtomicUsize::new(0));
        let fired2 = fired.clone();
        let mut c: TtlSortedCache<u32, u32> = TtlSortedCache::builder()
            .ttl(Duration::from_millis(20))
            .on_evict(move |_k: &u32, _v: &u32| {
                fired2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();
        c.cache_set(1, 100);
        std::thread::sleep(std::time::Duration::from_millis(60));
        // The previous value has expired: overwriting filters it (None), fires on_evict once,
        // and counts one eviction.
        assert_eq!(c.cache_set(1, 200), None);
        assert_eq!(c.cache_evictions(), Some(1));
        assert_eq!(fired.load(Ordering::Relaxed), 1);
        // Overwriting the now-live value returns it, no on_evict and no new eviction.
        assert_eq!(c.cache_set(1, 300), Some(200));
        assert_eq!(c.cache_evictions(), Some(1));
        assert_eq!(fired.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn entry_is_expired_at_uses_at_or_after_boundary() {
        // TtlSortedCache aligns with TtlCache / LruTtlCache: an entry is expired at-or-after its
        // deadline (`now >= expiry`), not strictly-after. The real monotonic clock never yields a
        // deterministic `now == expiry` tie, so exercise the boundary through `is_expired_at`.
        use super::{CacheArc, Entry};
        use crate::time::Instant;

        let deadline = Instant::now();
        let entry = Entry {
            expiry: Some(deadline),
            key: CacheArc::new(1u32),
            value: 42u32,
        };

        // Exactly at the deadline: expired (at-or-after). Strictly-after would report `false`.
        assert!(
            entry.is_expired_at(deadline),
            "entry must be expired at exactly its deadline (at-or-after)"
        );
        // One tick before: still live.
        assert!(
            !entry.is_expired_at(deadline - Duration::from_nanos(1)),
            "entry must be live just before its deadline"
        );
        // A never-expiring entry (zero-TTL sentinel) is never expired.
        let never = Entry {
            expiry: None,
            key: CacheArc::new(2u32),
            value: 7u32,
        };
        assert!(!never.is_expired_at(deadline));
    }

    #[test]
    fn set_with_ttl_overflow_stores_never_expiring_entry() {
        // set_with(..).ttl(..) with a Duration that would overflow Instant bounds stores
        // the entry with no expiry (never expires), matching cache_set on the other TTL
        // stores. No error surface: TtlSortedCache's Cached::Error is Infallible.
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        // Duration::MAX overflows Instant::now().checked_add -> None -> never expires.
        let prev = cache.set_with(1u32, 42u32).ttl(Duration::MAX).set();
        assert_eq!(prev, None);
        assert_eq!(cache.cache_size(), 1);
        assert_eq!(cache.cache_get(&1u32), Some(&42u32));
        // The entry survives an explicit eviction sweep (it never expires).
        assert_eq!(cache.evict(), 0);
        assert_eq!(cache.cache_get(&1u32), Some(&42u32));
    }

    #[derive(Clone, Debug)]
    struct CountingKey {
        label: &'static str,
        hash_calls: Arc<AtomicUsize>,
    }

    impl CountingKey {
        fn new(label: &'static str) -> Self {
            Self {
                label,
                hash_calls: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    impl Hash for CountingKey {
        fn hash<H: Hasher>(&self, state: &mut H) {
            self.hash_calls.fetch_add(1, Ordering::Relaxed);
            self.label.hash(state);
        }
    }

    impl PartialEq for CountingKey {
        fn eq(&self, other: &Self) -> bool {
            self.label == other.label
        }
    }

    impl Eq for CountingKey {}

    impl PartialOrd for CountingKey {
        fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
            Some(self.cmp(other))
        }
    }

    impl Ord for CountingKey {
        fn cmp(&self, other: &Self) -> CmpOrdering {
            self.label.cmp(other.label)
        }
    }

    #[test]
    fn new_returns_ready_cache_respecting_ttl() {
        use crate::CacheTtl;
        let mut c: TtlSortedCache<u32, u32> = TtlSortedCache::new(Duration::from_millis(50));
        assert_eq!(CacheTtl::ttl(&c), Some(Duration::from_millis(50)));
        c.cache_set(1, 100);
        assert_eq!(c.cache_get(&1), Some(&100));
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(c.cache_get(&1), None, "entry must expire after ttl");
        // No size bound from new().
        assert_eq!(c.cache_capacity(), None);
    }

    #[test]
    #[should_panic(expected = "non-zero ttl")]
    fn new_zero_ttl_panics() {
        let _c: TtlSortedCache<u32, u32> = TtlSortedCache::new(Duration::ZERO);
    }

    #[test]
    fn ttl_secs_and_ttl_millis_set_duration() {
        use crate::CacheTtl;
        let c: TtlSortedCache<u32, u32> = TtlSortedCache::builder().ttl_secs(7).build().unwrap();
        assert_eq!(CacheTtl::ttl(&c), Some(Duration::from_secs(7)));

        let c: TtlSortedCache<u32, u32> =
            TtlSortedCache::builder().ttl_millis(250).build().unwrap();
        assert_eq!(CacheTtl::ttl(&c), Some(Duration::from_millis(250)));
    }

    #[test]
    fn ttl_setters_override_last_writer_wins() {
        use crate::CacheTtl;
        // ttl(secs=10) then ttl_secs(5) -> 5s
        let c: TtlSortedCache<u32, u32> = TtlSortedCache::builder()
            .ttl(Duration::from_secs(10))
            .ttl_secs(5)
            .build()
            .unwrap();
        assert_eq!(CacheTtl::ttl(&c), Some(Duration::from_secs(5)));

        // ttl_secs then ttl_millis -> the millis value
        let c: TtlSortedCache<u32, u32> = TtlSortedCache::builder()
            .ttl_secs(10)
            .ttl_millis(500)
            .build()
            .unwrap();
        assert_eq!(CacheTtl::ttl(&c), Some(Duration::from_millis(500)));
    }

    #[test]
    fn borrow_keys() {
        let mut cache = TtlSortedCache::builder()
            .ttl(Duration::from_millis(100))
            .initial_capacity(100)
            .build()
            .unwrap();
        cache.set(String::from("a"), "a");
        assert_eq!(cache.get("a").unwrap(), &"a");

        let mut cache = TtlSortedCache::builder()
            .ttl(Duration::from_millis(100))
            .initial_capacity(100)
            .build()
            .unwrap();
        cache.set(vec![0], "a");
        assert_eq!(cache.get([0].as_slice()).unwrap(), &"a");
    }

    #[test]
    fn cache_get_live_hit_increments_hits() {
        let key = CountingKey::new("live");
        let mut cache = TtlSortedCache::builder()
            .ttl(Duration::from_secs(60))
            .initial_capacity(1)
            .build()
            .unwrap();
        cache.set(key.clone(), 10);

        assert_eq!(cache.cache_get(&key), Some(&10));
        assert_eq!(cache.cache_hits(), Some(1));
        assert_eq!(cache.cache_misses(), Some(0));
        assert_eq!(cache.cache_size(), 1);
        assert_eq!(cache.keys.len(), 1);
    }

    #[test]
    fn cache_get_mut_live_hit_updates_value() {
        let key = CountingKey::new("live-mut");
        let mut cache = TtlSortedCache::builder()
            .ttl(Duration::from_secs(60))
            .initial_capacity(1)
            .build()
            .unwrap();
        cache.set(key.clone(), 10);

        let value = cache.cache_get_mut(&key).expect("entry should be live");
        *value = 11;

        assert_eq!(cache.cache_hits(), Some(1));
        assert_eq!(cache.cache_misses(), Some(0));
        assert_eq!(cache.cache_get(&key), Some(&11));
    }

    #[test]
    fn cache_get_expired_hit_removes_map_and_ttl_index() {
        let evicted = Arc::new(AtomicUsize::new(0));
        let evicted_clone = evicted.clone();
        let mut cache = TtlSortedCache::builder()
            .ttl(Duration::from_secs(60))
            .on_evict(move |k: &&str, v: &u32| {
                assert_eq!(*k, "expired");
                assert_eq!(*v, 10);
                evicted_clone.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .expect("cache should build");

        // Use a very short but non-zero TTL (zero now means "never expires").
        cache
            .set_with("expired", 10)
            .ttl(Duration::from_millis(1))
            .set();
        assert_eq!(cache.cache_size(), 1);
        assert_eq!(cache.keys.len(), 1);

        // Wait for the TTL to elapse before querying.
        std::thread::sleep(std::time::Duration::from_millis(20));

        assert_eq!(cache.cache_get(&"expired"), None);

        assert_eq!(cache.cache_size(), 0);
        assert_eq!(cache.keys.len(), 0);
        assert_eq!(cache.cache_hits(), Some(0));
        assert_eq!(cache.cache_misses(), Some(1));
        assert_eq!(cache.cache_evictions(), Some(1));
        assert_eq!(evicted.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn cache_get_mut_expired_hit_removes_map_and_ttl_index() {
        let evicted = Arc::new(AtomicUsize::new(0));
        let evicted_clone = evicted.clone();
        let mut cache = TtlSortedCache::builder()
            .ttl(Duration::from_secs(60))
            .on_evict(move |k: &&str, v: &u32| {
                assert_eq!(*k, "expired-mut");
                assert_eq!(*v, 20);
                evicted_clone.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .expect("cache should build");

        // Use a very short but non-zero TTL (zero now means "never expires").
        cache
            .set_with("expired-mut", 20)
            .ttl(Duration::from_millis(1))
            .set();
        assert_eq!(cache.cache_size(), 1);
        assert_eq!(cache.keys.len(), 1);

        // Wait for the TTL to elapse before querying.
        std::thread::sleep(std::time::Duration::from_millis(20));

        assert_eq!(cache.cache_get_mut(&"expired-mut"), None);

        assert_eq!(cache.cache_size(), 0);
        assert_eq!(cache.keys.len(), 0);
        assert_eq!(cache.cache_hits(), Some(0));
        assert_eq!(cache.cache_misses(), Some(1));
        assert_eq!(cache.cache_evictions(), Some(1));
        assert_eq!(evicted.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn kitchen_sink() {
        let mut cache = TtlSortedCache::builder()
            .ttl(Duration::from_millis(100))
            .initial_capacity(100)
            .build()
            .unwrap();
        assert_eq!(0, cache.evict());
        assert_eq!(0, cache.retain_latest(100, true));
        assert!(cache.get("a").is_none());

        cache.set("a".to_string(), "A".to_string());
        assert_eq!(cache.get("a"), Some("A".to_string()).as_ref());
        assert_eq!(cache.len(), 1);
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(1, cache.evict());
        assert!(cache.get("a").is_none());
        assert_eq!(cache.len(), 0);

        cache.set("a".to_string(), "A".to_string());
        assert_eq!(cache.get("a"), Some("A".to_string()).as_ref());
        assert_eq!(cache.len(), 1);
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(0, cache.retain_latest(1, false));
        // Expired-but-not-yet-evicted: use the non-mutating read so the entry
        // stays in the map (the next assertion verifies it's still counted).
        assert_eq!(cache.cache_get_read("a"), None);
        // in size until eviction
        assert_eq!(cache.len(), 1);
        assert_eq!(1, cache.retain_latest(1, true));
        assert!(cache.get("a").is_none());
        assert_eq!(cache.len(), 0);

        cache.set("a".to_string(), "a".to_string());
        cache.set("b".to_string(), "b".to_string());
        cache.set("c".to_string(), "c".to_string());
        cache.set("d".to_string(), "d".to_string());
        cache.set("e".to_string(), "e".to_string());
        assert_eq!(3, cache.retain_latest(2, false));
        assert_eq!(2, cache.len());
        assert_eq!(cache.get("a"), None);
        assert_eq!(cache.get("b"), None);
        assert_eq!(cache.get("c"), None);
        assert_eq!(cache.get("d"), Some("d".to_string()).as_ref());
        assert_eq!(cache.get("e"), Some("e".to_string()).as_ref());

        cache.set("a".to_string(), "a".to_string());
        cache.set("a".to_string(), "a".to_string());
        cache.set("b".to_string(), "b".to_string());
        cache.set("b".to_string(), "b".to_string());
        assert_eq!(4, cache.len());

        assert_eq!(2, cache.retain_latest(2, false));
        assert_eq!(cache.get("d"), None);
        assert_eq!(cache.get("e"), None);
        assert_eq!(cache.get("a"), Some("a".to_string()).as_ref());
        assert_eq!(cache.get("b"), Some("b".to_string()).as_ref());
        assert_eq!(2, cache.len());

        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(cache.remove("a"), None);
        // trying to get something expired will expire values
        assert_eq!(1, cache.len());

        cache.set("a".to_string(), "a".to_string());
        assert_eq!(cache.remove("a"), Some("a".to_string()));
        // we haven't done anything to evict "b" so there's still one entry
        assert_eq!(1, cache.len());

        assert_eq!(1, cache.evict());
        assert_eq!(0, cache.len());

        // default ttl is 100ms
        cache
            .set_with("a".to_string(), "a".to_string())
            .ttl(Duration::from_millis(300))
            .set();
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(cache.get("a"), Some("a".to_string()).as_ref());
        assert_eq!(1, cache.len());

        std::thread::sleep(Duration::from_millis(200));
        cache
            .set_with("b".to_string(), "b".to_string())
            .ttl(Duration::from_millis(300))
            .evict()
            .set();
        // a should now be evicted
        assert_eq!(1, cache.len());
        assert_eq!(cache.get("a"), None);
    }

    #[test]
    fn set_max_size() {
        let mut cache = TtlSortedCache::builder()
            .ttl(Duration::from_millis(100))
            .initial_capacity(100)
            .build()
            .unwrap();
        cache.set_max_size(2);
        assert_eq!(0, cache.evict());
        assert_eq!(0, cache.retain_latest(100, true));
        assert!(cache.get("a").is_none());

        cache.set("a".to_string(), "A".to_string());
        assert_eq!(cache.get("a"), Some("A".to_string()).as_ref());
        assert_eq!(cache.len(), 1);
        cache.set("b".to_string(), "B".to_string());
        assert_eq!(cache.get("b"), Some("B".to_string()).as_ref());
        assert_eq!(cache.len(), 2);
        cache.set("c".to_string(), "C".to_string());
        assert_eq!(cache.len(), 2);
        assert_eq!(cache.get("b"), Some("B".to_string()).as_ref());
        assert_eq!(cache.get("c"), Some("C".to_string()).as_ref());
        assert_eq!(cache.get("a"), None);
    }

    #[test]
    fn capacity_returns_bound_not_live_size() {
        let cache: TtlSortedCache<u32, u32> = TtlSortedCache::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        assert_eq!(cache.capacity(), None);
        assert_eq!(cache.capacity(), cache.cache_capacity());

        let mut cache = TtlSortedCache::builder()
            .ttl(Duration::from_secs(60))
            .max_size(3)
            .build()
            .unwrap();
        assert_eq!(cache.capacity(), Some(3));
        assert_eq!(cache.capacity(), cache.cache_capacity());

        cache.cache_set(1, 10);
        cache.cache_set(2, 20);
        assert_eq!(
            cache.capacity(),
            Some(3),
            "capacity tracks the bound, not live entries"
        );
        assert_eq!(cache.len(), 2);

        cache.set_max_size(5);
        assert_eq!(cache.capacity(), Some(5));
        assert_eq!(cache.capacity(), cache.cache_capacity());
    }

    #[test]
    fn updating_existing_key_at_size_limit_does_not_evict_another_key() {
        let mut cache = TtlSortedCache::builder()
            .ttl(Duration::from_millis(1_000))
            .initial_capacity(2)
            .build()
            .unwrap();
        cache.set_max_size(2);

        cache.set("a".to_string(), "A".to_string());
        cache.set("b".to_string(), "B".to_string());
        assert_eq!(cache.len(), 2);

        assert_eq!(
            cache.set("a".to_string(), "A2".to_string()),
            Some("A".to_string())
        );
        assert_eq!(cache.len(), 2);
        assert_eq!(cache.get("a"), Some(&"A2".to_string()));
        assert_eq!(cache.get("b"), Some(&"B".to_string()));
        assert_eq!(cache.cache_evictions(), Some(0));
    }

    #[test]
    fn builder_rejects_zero_size_limit() {
        let cache = TtlSortedCache::<String, String>::builder()
            .ttl(Duration::from_millis(1_000))
            .max_size(0)
            .build();
        match cache {
            Ok(_) => panic!("zero size limit should fail"),
            Err(error) => assert!(
                matches!(error, crate::stores::BuildError::InvalidValue { .. }),
                "expected InvalidValue, got {error:?}"
            ),
        }
    }

    #[test]
    fn try_set_max_size_rejects_zero() {
        let mut cache = TtlSortedCache::<String, String>::builder()
            .ttl(Duration::from_millis(1_000))
            .build()
            .unwrap();
        assert_eq!(
            cache.try_set_max_size(0),
            Err(super::super::SetMaxSizeError::ZeroMaxSize)
        );
        assert_eq!(cache.try_set_max_size(5).unwrap(), None);
    }

    #[test]
    #[should_panic(expected = "max_size must be greater than zero")]
    fn set_max_size_zero_panics() {
        let mut cache = TtlSortedCache::<String, String>::builder()
            .ttl(Duration::from_millis(1_000))
            .build()
            .unwrap();
        cache.set_max_size(0);
    }

    // CORE-1: a capacity-overflowing `max_size` must return `Err(BuildError)`
    // from the fallible `try_reserve`, not abort, matching the LRU-family stores.
    #[test]
    fn build_rejects_capacity_overflowing_max_size() {
        let cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .max_size(usize::MAX)
            .build();
        match cache {
            Ok(_) => panic!("usize::MAX max_size should fail to allocate"),
            Err(error) => assert!(
                matches!(error, crate::stores::BuildError::InvalidValue { .. }),
                "expected InvalidValue, got {error:?}"
            ),
        }
    }

    #[test]
    fn build_rejects_capacity_overflowing_initial_capacity() {
        let cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .initial_capacity(usize::MAX)
            .build();
        assert!(
            matches!(cache, Err(crate::stores::BuildError::InvalidValue { .. })),
            "usize::MAX initial_capacity should return Err(InvalidValue)"
        );
    }

    // CORE-1: `set_max_size` grows on demand (no pre-reserve), so even a huge
    // bound cannot panic; the documented panic-free `try_set_max_size` holds.
    #[test]
    fn try_set_max_size_huge_does_not_panic() {
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        assert_eq!(cache.try_set_max_size(usize::MAX).unwrap(), None);
    }

    #[test]
    fn explicit_capacity_takes_precedence_over_max_size_preallocation() {
        // Regression for #266: an explicit, smaller `capacity` must not be defeated
        // by `max_size`'s `size + 1` preallocation (HashMap::reserve does not reduce
        // an existing allocation).
        let cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(300))
            .max_size(65_536)
            .initial_capacity(16)
            .build()
            .unwrap();
        // The backing map must not have taken the max_size path, which would reserve
        // for max_size + 1 (= 65_537) entries.
        assert!(
            cache.map.capacity() < 65_537,
            "expected the explicit initial_capacity(16) to take precedence, got {}",
            cache.map.capacity()
        );
        assert!(cache.map.capacity() >= 16);
        // The eviction bound still reflects max_size.
        assert_eq!(cache.size_limit, Some(65_536));
    }

    #[test]
    fn max_size_alone_preallocates() {
        // Without an explicit capacity, max_size still drives preallocation.
        let cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(300))
            .max_size(64)
            .build()
            .unwrap();
        assert!(cache.map.capacity() >= 65);
    }

    #[test]
    fn get_or_set_with_max_size_limit_short_ttl_does_not_panic() {
        // Regression: when the just-inserted entry expires before existing entries,
        // `retain_latest` must evict the existing entry, not the one we're returning.
        let mut cache = TtlSortedCache::builder()
            .ttl(Duration::from_millis(1))
            .build()
            .unwrap();
        cache.set_max_size(1);
        cache
            .set_with("long", 1u32)
            .ttl(Duration::from_secs(60))
            .set();
        // Must not panic; "long" should be evicted to make room for "short".
        let v = cache.cache_get_or_set_with("short", || 2u32);
        assert_eq!(*v, 2);
        // Size limit must be respected after the call.
        assert_eq!(cache.cache_size(), 1);
        // "short" is the entry that survived; "long" was evicted.
        assert_eq!(cache.cache_get("short"), Some(&2u32));
    }

    #[test]
    fn try_get_or_set_with_max_size_limit_short_ttl_does_not_panic() {
        // Regression: same scenario as `get_or_set_with_max_size_limit_short_ttl_does_not_panic`
        // but via the fallible `cache_try_get_or_set_with` path, which also routes through
        // `set_and_get_mut`.
        let mut cache = TtlSortedCache::builder()
            .ttl(Duration::from_millis(1))
            .build()
            .unwrap();
        cache.set_max_size(1);
        cache
            .set_with("long", 1u32)
            .ttl(Duration::from_secs(60))
            .set();
        let v: &mut u32 = cache
            .cache_try_get_or_set_with_mut("short", || Ok::<u32, ()>(2))
            .unwrap();
        assert_eq!(*v, 2);
        assert_eq!(cache.cache_size(), 1);
        assert_eq!(cache.cache_get("short"), Some(&2u32));
    }

    #[test]
    fn shared_ref_get_or_set_with_wrapper_delegates_to_mut() {
        // The `&V`-returning `cache_get_or_set_with` / `cache_try_get_or_set_with`
        // are provided as defaults that delegate to the `_mut` variants. Exercise
        // them directly (not the `_mut` methods) so the delegation stays covered.
        let mut cache: TtlSortedCache<&str, u32> = TtlSortedCache::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();

        let v: &u32 = cache.cache_get_or_set_with("a", || 1u32);
        assert_eq!(*v, 1);

        let v: &u32 = cache
            .cache_try_get_or_set_with("b", || Ok::<u32, ()>(2))
            .unwrap();
        assert_eq!(*v, 2);

        // Hit path: the closure must not run, and the stored value is returned by `&V`.
        let v: &u32 = cache.cache_get_or_set_with("a", || 99u32);
        assert_eq!(*v, 1);
        assert_eq!(cache.cache_size(), 2);
    }

    #[cfg(feature = "async")]
    #[tokio::test]
    async fn async_cache_get_or_set_with_max_size_limit_short_ttl_does_not_panic() {
        use crate::CachedGetOrSetAsync;
        let mut cache = TtlSortedCache::builder()
            .ttl(Duration::from_millis(1))
            .build()
            .unwrap();
        cache.set_max_size(1);
        cache
            .set_with("long", 1u32)
            .ttl(Duration::from_secs(60))
            .set();
        let v = cache
            .async_cache_get_or_set_with("short", || async { 2u32 })
            .await;
        assert_eq!(*v, 2);
        assert_eq!(cache.cache_size(), 1);
        // "long" was evicted by the size limit (not by TTL expiry); verify it is gone.
        // Asserting cache_get("short") would be racy: the 1ms TTL can expire between
        // the .await resumption and this line under a loaded CI runner.
        assert_eq!(
            cache.cache_get("long"),
            None,
            "long entry should have been evicted"
        );
    }

    #[test]
    fn cache_clear_with_on_evict_fires_for_all_entries() {
        let count = Arc::new(AtomicUsize::new(0));
        let count2 = count.clone();
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .on_evict(move |_k: &u32, _v: &u32| {
                count2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();
        cache.cache_set(1, 10);
        cache.cache_set(2, 20);
        cache.cache_set(3, 30);
        cache.cache_clear_with_on_evict();
        assert_eq!(cache.cache_size(), 0);
        assert_eq!(cache.keys.len(), 0);
        assert_eq!(count.load(Ordering::Relaxed), 3);
        assert_eq!(cache.cache_evictions(), Some(3));
    }

    #[test]
    fn cache_clear_does_not_fire_on_evict() {
        let count = Arc::new(AtomicUsize::new(0));
        let count2 = count.clone();
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .on_evict(move |_k: &u32, _v: &u32| {
                count2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();
        cache.cache_set(1, 10);
        cache.cache_set(2, 20);
        cache.cache_clear();
        assert_eq!(cache.cache_size(), 0);
        assert_eq!(
            count.load(Ordering::Relaxed),
            0,
            "cache_clear must not fire on_evict"
        );
    }

    #[test]
    fn cache_reset_preserves_configuration() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU64, Ordering};

        let evicted = Arc::new(AtomicU64::new(0));
        let evicted_clone = evicted.clone();

        let mut cache = TtlSortedCache::<u8, u8>::builder()
            .ttl(Duration::from_secs(60))
            .max_size(2)
            .on_evict(move |_k: &u8, _v: &u8| {
                evicted_clone.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .expect("build failed");

        cache.cache_set(1, 1);
        cache.cache_set(2, 2);
        cache.cache_reset();
        assert_eq!(0, cache.cache_size(), "reset should clear all entries");

        // After reset, size_limit and on_evict must still be active.
        cache.cache_set(3, 3);
        cache.cache_set(4, 4);
        cache.cache_set(5, 5); // capacity-2 → evicts one entry
        assert_eq!(2, cache.cache_size(), "size limit should still be enforced");
        assert_eq!(
            1,
            evicted.load(Ordering::Relaxed),
            "on_evict should still fire after reset"
        );
    }

    // CORE-8: `cache_reset` shrinks back to the build-time preallocation hint,
    // not to zero, so the configured capacity survives a reset.
    #[test]
    fn cache_reset_preserves_initial_capacity() {
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .initial_capacity(128)
            .build()
            .unwrap();
        for i in 0..64 {
            cache.cache_set(i, i);
        }
        cache.cache_reset();
        assert!(
            cache.map.capacity() >= 128,
            "reset should retain the initial_capacity hint, got {}",
            cache.map.capacity()
        );
    }

    #[test]
    fn test_diagnostics_and_traits() {
        let mut cache = TtlSortedCache::builder()
            .ttl(Duration::from_secs(60))
            .max_size(3)
            .build()
            .unwrap();
        cache.cache_set(1, 100);
        cache.cache_set(2, 200);

        // Debug
        let debug_str = format!("{:?}", cache);
        assert!(debug_str.contains("TtlSortedCache"));
        assert!(debug_str.contains("ttl"));
        assert!(debug_str.contains("size_limit"));
        assert!(debug_str.contains("hits"));
        assert!(debug_str.contains("misses"));

        // Clone
        let mut cloned = cache.clone();
        assert_eq!(cloned.cache_get(&1), Some(&100));
        assert_eq!(cloned.cache_get(&2), Some(&200));

        // Builder build errors
        let builder = TtlSortedCache::<u32, u32>::builder();
        let built = builder.build();
        assert!(built.is_err()); // Missing required ttl

        let builder = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .max_size(0);
        let built = builder.build();
        assert!(built.is_err()); // Size limit 0 is invalid

        let builder = TtlSortedCache::<u32, u32>::builder().ttl(Duration::ZERO);
        let built = builder.build();
        assert!(built.is_err()); // Zero ttl is invalid
    }

    #[test]
    fn cache_remove_entry_returns_some_for_live_entry() {
        let mut c = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        c.cache_set(1u32, 100u32);
        let removed = c.cache_remove_entry(&1u32);
        assert_eq!(removed, Some((1u32, 100u32)));
        assert_eq!(c.cache_size(), 0);
    }

    #[test]
    fn cache_remove_entry_returns_some_for_expired_entry() {
        let mut c = TtlSortedCache::<u32, u32>::builder()
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
        assert_eq!(
            removed.expect("cache_remove_entry must return Some for expired entry"),
            (2u32, 200u32)
        );
    }

    #[test]
    fn cache_delete_returns_true_for_expired_entry() {
        let mut c = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_millis(50))
            .build()
            .unwrap();
        c.cache_set(1u32, 100u32);
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(
            c.cache_delete(&1u32),
            "cache_delete must return true even for expired entry"
        );
        assert!(
            !c.cache_delete(&1u32),
            "cache_delete returns false when absent"
        );
    }

    #[test]
    fn cache_remove_entry_fires_on_evict_for_expired() {
        let count = Arc::new(AtomicUsize::new(0));
        let count2 = count.clone();
        let mut c = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_millis(50))
            .on_evict(move |_k, _v| {
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
    fn cache_remove_entry_absent_returns_none() {
        let mut c = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        assert_eq!(c.cache_remove_entry(&42u32), None);
    }

    #[test]
    fn cache_remove_entry_increments_eviction_counter() {
        let mut c = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_millis(10))
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

    // ── Item 3: set_ttl(0) = "never expires" behavioral tests ─────────────

    /// Zero TTL at insert time means entries NEVER expire (not "expire immediately").
    #[test]
    fn set_ttl_zero_entries_never_expire() {
        use crate::CacheTtl;
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_millis(50))
            .build()
            .unwrap();
        // Switch to zero TTL before inserting.
        cache.set_ttl(Duration::ZERO);
        cache.cache_set(1u32, 10u32);
        // Wait well past the original 50ms TTL.
        std::thread::sleep(std::time::Duration::from_millis(150));
        // Entry must still be present (never expires).
        assert_eq!(
            cache.cache_get(&1u32),
            Some(&10u32),
            "entry inserted with zero TTL must never expire"
        );
        // ttl() resolves the zero TTL to None (expiry disabled), like the sibling stores.
        assert_eq!(CacheTtl::ttl(&cache), None);
    }

    /// Switching set_ttl to zero only affects entries inserted AFTER the change.
    /// Pre-existing finite-expiry entries still expire on their original schedule.
    #[test]
    fn set_ttl_zero_only_affects_future_inserts() {
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_millis(80))
            .build()
            .unwrap();
        // Insert with the current finite TTL.
        cache.cache_set(1u32, 100u32);
        // Switch to zero TTL (never-expires) for future inserts.
        cache.set_ttl(Duration::ZERO);
        cache.cache_set(2u32, 200u32);
        // Wait past the finite TTL for key 1.
        std::thread::sleep(std::time::Duration::from_millis(150));
        // Key 1 (finite TTL) must be expired.
        assert_eq!(
            cache.cache_get(&1u32),
            None,
            "pre-existing finite-TTL entry must expire"
        );
        // Key 2 (inserted with zero TTL = never expires) must still be present.
        assert_eq!(
            cache.cache_get(&2u32),
            Some(&200u32),
            "entry inserted with zero TTL must never expire"
        );
    }

    /// Under size pressure, never-expiring entries (None expiry) are evicted LAST —
    /// after all finite-expiry entries have been dropped.
    #[test]
    fn set_ttl_zero_never_expire_entries_evicted_last_under_size_pressure() {
        // Build with max_size = 2.
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(10))
            .max_size(2)
            .build()
            .unwrap();

        // Insert one never-expiring entry.
        cache.set_ttl(Duration::ZERO);
        cache.cache_set(1u32, 10u32);

        // Insert two finite-TTL entries (these must be evicted before the never-expiring one).
        cache.set_ttl(Duration::from_millis(500));
        cache.cache_set(2u32, 20u32);
        cache.cache_set(3u32, 30u32);
        // At this point the cache has 3 entries and max_size = 2; key 1 (never-expiring, None
        // expiry, sorts greatest) must be the survivor along with the later finite entry.
        // Actually, retain_latest evicts the soonest-expiring first: key 2 and key 3 have
        // Some(expiry) and key 1 has None (greatest). So one of key 2/3 was evicted, and
        // key 1 (never-expires) survives.
        assert_eq!(cache.cache_size(), 2, "max_size must be enforced");
        assert_eq!(
            cache.cache_get(&1u32),
            Some(&10u32),
            "never-expiring entry must survive size eviction"
        );

        // Now insert one more to push out the remaining finite-expiry entry.
        cache.cache_set(4u32, 40u32);
        assert_eq!(cache.cache_size(), 2);
        assert_eq!(
            cache.cache_get(&1u32),
            Some(&10u32),
            "never-expiring entry must still survive"
        );
    }

    /// unset_ttl is equivalent to set_ttl(Duration::ZERO): future inserts never expire.
    #[test]
    fn unset_ttl_makes_future_inserts_never_expire() {
        use crate::CacheTtl;
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_millis(50))
            .build()
            .unwrap();
        cache.unset_ttl();
        assert_eq!(
            CacheTtl::ttl(&cache),
            None,
            "unset_ttl disables expiry, so ttl() resolves to None"
        );
        cache.cache_set(1u32, 99u32);
        std::thread::sleep(std::time::Duration::from_millis(120));
        assert_eq!(
            cache.cache_get(&1u32),
            Some(&99u32),
            "entry inserted after unset_ttl must never expire"
        );
    }

    /// Evict must not sweep never-expiring (None expiry) entries.
    #[test]
    fn evict_does_not_remove_never_expiring_entries() {
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_millis(20))
            .build()
            .unwrap();
        // Insert a finite-TTL entry.
        cache.cache_set(1u32, 10u32);
        // Switch to zero TTL and insert a never-expiring entry.
        cache.set_ttl(Duration::ZERO);
        cache.cache_set(2u32, 20u32);
        // Wait for the finite entry to expire.
        std::thread::sleep(std::time::Duration::from_millis(80));
        let evicted = cache.evict();
        // Only the finite-TTL entry should be swept.
        assert_eq!(
            evicted, 1,
            "evict must sweep only expired finite-TTL entries"
        );
        assert_eq!(cache.cache_size(), 1, "never-expiring entry must remain");
        assert_eq!(cache.cache_get(&2u32), Some(&20u32));
    }

    /// A `Drop` panic mid-sweep must not orphan entries. In the no-callback branch of
    /// `evict_at`, the whole expired prefix is detached from `self.keys` up front (one
    /// `split_off`); the map rows are then removed one at a time. If a value's (or key's)
    /// `Drop` panics before every row has left `self.map`, the not-yet-removed rows are
    /// orphaned: their stamps are already gone from `self.keys`, so a later sweep never
    /// reaches them, yet `len` still counts them. The fix drains `self.map` fully into a
    /// local `Vec` and lets the values drop only after the drain, matching the callback
    /// branch, so `self.map` and `self.keys` stay in lockstep and the eviction counter
    /// reflects exactly what was pulled from the map — even across a panicking `Drop`.
    #[test]
    fn evict_no_callback_keeps_map_and_keys_in_lockstep_on_drop_panic() {
        use std::panic::{AssertUnwindSafe, catch_unwind};

        // A value whose `Drop` panics only when armed. Exactly ONE instance is armed so
        // the unwinding drop of the drain `Vec` (a slice drop continues dropping the
        // remaining elements after one panics) never double-panics into a process abort.
        struct PanicOnDrop {
            armed: bool,
        }
        impl Drop for PanicOnDrop {
            fn drop(&mut self) {
                if self.armed {
                    panic!("PanicOnDrop::drop fired");
                }
            }
        }

        // No `on_evict` configured, so `evict()` takes the no-callback branch of `evict_at`.
        let mut cache: TtlSortedCache<u32, PanicOnDrop> = TtlSortedCache::builder()
            .ttl(Duration::from_millis(5))
            .build()
            .unwrap();

        // Insert the armed value FIRST: sequential inserts give ascending expiry stamps and
        // `Stamped` orders by (expiry, key), so key 0 is the earliest in the sweep and its
        // `Drop` runs before the two later rows have left `self.map`. On the pre-fix
        // per-iteration-drop code that panic orphans keys 1 and 2 in `self.map` while their
        // stamps are already gone from `self.keys`.
        cache.set_with(0u32, PanicOnDrop { armed: true }).set();
        cache.set_with(1u32, PanicOnDrop { armed: false }).set();
        cache.set_with(2u32, PanicOnDrop { armed: false }).set();
        assert_eq!(cache.map.len(), 3);
        assert_eq!(cache.keys.len(), 3);

        // Let the whole 5ms-TTL prefix expire, then sweep. The armed `Drop` panics mid-sweep.
        std::thread::sleep(std::time::Duration::from_millis(40));
        let result = catch_unwind(AssertUnwindSafe(|| {
            let _ = cache.evict();
        }));
        assert!(
            result.is_err(),
            "the armed value's Drop must have panicked during the sweep"
        );

        // Lockstep is the core property: whatever left `self.keys` must also have left
        // `self.map`. Pre-fix this failed (map.len() == 2 vs keys.len() == 0: two orphans).
        assert_eq!(
            cache.map.len(),
            cache.keys.len(),
            "map and keys must stay in lockstep across a Drop panic (no orphaned rows)"
        );
        // Every entry had expired, so the sweep drained the whole prefix from both.
        assert_eq!(
            cache.map.len(),
            0,
            "all expired entries must have left the map"
        );
        assert_eq!(cache.cache_size(), 0);
        // The counter is incremented before the drain `Vec` drops, so it reflects exactly
        // the three rows pulled from the map. Pre-fix the batched `fetch_add` after the loop
        // was skipped by the panic, leaving it at 0.
        assert_eq!(
            cache.cache_evictions(),
            Some(3),
            "eviction counter must match the rows actually removed from the map"
        );
    }

    /// `set_with(..).ttl(..)` called with an EXPLICIT `Duration::ZERO` (not the cache-level
    /// `set_ttl`) must store `expiry = None` (never expires), not `Some(now)`
    /// (immediate). The cache's default TTL stays finite the whole time.
    #[test]
    fn set_with_ttl_explicit_zero_never_expires() {
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_millis(20))
            .build()
            .unwrap();
        // Explicit zero TTL on this one entry — default ttl remains 20ms.
        cache.set_with(1u32, 10u32).ttl(Duration::ZERO).set();
        // The entry's internal expiry must be None (never), not Some(now).
        assert!(
            cache
                .map
                .get(&1u32)
                .expect("entry present")
                .expiry
                .is_none(),
            "explicit Duration::ZERO must store expiry = None (never expires)"
        );
        // Wait far past the default 20ms TTL.
        std::thread::sleep(std::time::Duration::from_millis(80));
        assert_eq!(
            cache.cache_get(&1u32),
            Some(&10u32),
            "entry inserted with explicit zero TTL must never expire"
        );
        // A sibling inserted with the finite default TTL must still expire.
        cache.cache_set(2u32, 20u32);
        std::thread::sleep(std::time::Duration::from_millis(80));
        assert_eq!(
            cache.cache_get(&2u32),
            None,
            "finite-TTL sibling must expire"
        );
        assert_eq!(cache.cache_get(&1u32), Some(&10u32));
    }

    /// `set_with(..).ttl(..).evict()` with explicit `Duration::ZERO` also stores `None`,
    /// and the never-expiring entry is not swept by the eviction pass it triggers.
    #[test]
    fn set_with_ttl_evict_explicit_zero_never_expires_and_survives_evict() {
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_millis(10))
            .build()
            .unwrap();
        // A finite, soon-to-expire entry.
        cache.cache_set(1u32, 10u32);
        std::thread::sleep(std::time::Duration::from_millis(40));
        // Insert a never-expiring entry AND run the eviction pass in the same call.
        cache
            .set_with(2u32, 20u32)
            .ttl(Duration::ZERO)
            .evict()
            .set();
        assert!(
            cache
                .map
                .get(&2u32)
                .expect("entry present")
                .expiry
                .is_none(),
            "explicit zero TTL must be None"
        );
        // The expired finite entry was swept; the never-expiring one survives.
        assert_eq!(cache.cache_get(&1u32), None, "expired entry swept by evict");
        assert_eq!(
            cache.cache_get(&2u32),
            Some(&20u32),
            "never-expiring entry must survive its own evict pass"
        );
    }

    /// `set_with(k, v).set()` with no `.ttl()`/`.evict()` calls must behave exactly like
    /// plain `set`: same default TTL, same displaced-value return, no eviction sweep.
    #[test]
    fn set_with_default_matches_plain_set() {
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_millis(200))
            .build()
            .unwrap();

        // First insert via the builder with no overrides: no previous value.
        assert_eq!(cache.set_with(1u32, 10u32).set(), None);
        assert_eq!(cache.cache_get(&1u32), Some(&10u32));

        // Overwriting the still-live entry returns the displaced value, matching `set`.
        assert_eq!(cache.set_with(1u32, 11u32).set(), Some(10u32));
        assert_eq!(cache.cache_get(&1u32), Some(&11u32));

        // The entry uses the cache's default TTL (no override was applied): it expires
        // after the configured 200ms, exactly like a plain `set`.
        std::thread::sleep(std::time::Duration::from_millis(260));
        assert_eq!(
            cache.cache_get(&1u32),
            None,
            "set_with with no .ttl() override must use the cache's default TTL"
        );
    }

    /// `.ttl(d)` overrides the store's default TTL for that single entry: a shorter
    /// override expires before the cache default would, and a longer override survives
    /// past the point a default-TTL sibling has already expired.
    #[test]
    fn set_with_ttl_overrides_default_ttl() {
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_millis(500))
            .build()
            .unwrap();

        // Shorter override: expires well before the 500ms default would.
        cache
            .set_with(1u32, 10u32)
            .ttl(Duration::from_millis(20))
            .set();
        std::thread::sleep(std::time::Duration::from_millis(80));
        assert_eq!(
            cache.cache_get(&1u32),
            None,
            "a shorter .ttl() override must expire before the cache default TTL"
        );

        // Longer override: still live after the cache's own default would have expired
        // a plain entry (proving the per-entry override, not the default, is in effect).
        cache
            .set_with(2u32, 20u32)
            .ttl(Duration::from_secs(60))
            .set();
        cache.cache_set(3u32, 30u32); // default-TTL sibling, 500ms
        std::thread::sleep(std::time::Duration::from_millis(600));
        assert_eq!(
            cache.cache_get(&3u32),
            None,
            "default-TTL sibling must have expired by now"
        );
        assert_eq!(
            cache.cache_get(&2u32),
            Some(&20u32),
            "a longer .ttl() override must survive past the default TTL window"
        );
    }

    /// `.evict()` opts into running the size-limit eviction pass after insertion: once the
    /// bound is exceeded, the next-to-expire entry is dropped as part of the `.set()` call
    /// that requested it. Without `.evict()`, `set_with` still enforces `size_limit`
    /// (size-limit enforcement is unconditional in `set_inner`; `.evict()` only affects the
    /// TTL-sweep-when-no-size-limit path), so this test also checks the no-size-limit case
    /// via `evict()` triggering a plain expiry sweep.
    #[test]
    fn set_with_evict_triggers_eviction() {
        // Case 1: no size_limit configured — `.evict()` runs a TTL sweep as part of `.set()`.
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_millis(10))
            .build()
            .unwrap();
        cache.cache_set(1u32, 10u32);
        std::thread::sleep(std::time::Duration::from_millis(40));
        assert_eq!(cache.cache_evictions(), Some(0));
        // Insert a second entry and opt into eviction: the expired entry (key 1) must be
        // swept as part of this call.
        cache.set_with(2u32, 20u32).evict().set();
        assert_eq!(
            cache.cache_evictions(),
            Some(1),
            ".evict() must run the TTL sweep as part of set_with(..).set()"
        );

        // Case 2: with a size_limit configured, exceeding it evicts the next-to-expire
        // entry regardless of `.evict()`; migrated from the historical `set_evict` coverage
        // (kitchen_sink's "a"/"b" scenario), isolated here for the builder specifically.
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .max_size(1)
            .build()
            .unwrap();
        cache.set_with(1u32, 10u32).set();
        assert_eq!(cache.cache_size(), 1);
        cache.set_with(2u32, 20u32).evict().set();
        assert_eq!(
            cache.cache_size(),
            1,
            "size_limit must still cap entries when inserting via set_with(..).evict()"
        );
        assert_eq!(cache.cache_get(&1u32), None, "next-to-expire entry evicted");
        assert_eq!(cache.cache_get(&2u32), Some(&20u32));
    }

    /// The terminal `.set()` returns the displaced unexpired value on overwrite (`Some`),
    /// and `None` when the previous entry was absent or had already expired.
    #[test]
    fn set_with_set_returns_displaced_value() {
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_millis(50))
            .build()
            .unwrap();

        // Absent key: None.
        assert_eq!(cache.set_with(1u32, 10u32).set(), None);

        // Overwrite of a live entry: Some(previous value).
        assert_eq!(cache.set_with(1u32, 11u32).set(), Some(10u32));

        // Let the entry expire, then overwrite: the expired value is filtered to None.
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(
            cache.set_with(1u32, 12u32).set(),
            None,
            "overwriting an expired entry must return None, not the stale value"
        );
        assert_eq!(cache.cache_get(&1u32), Some(&12u32));
    }

    /// `retain_latest` over a MIX of never-expires (`None`) and finite (`Some`) entries:
    /// finite entries are popped first (soonest-expiry order); `None` entries are retained
    /// last regardless of insertion order. Verified across several `count` values.
    #[test]
    fn retain_latest_keeps_never_expiring_entries_last() {
        // Insertion order deliberately interleaves never/finite to prove that ordering,
        // not insertion order, decides eviction.
        fn fresh() -> TtlSortedCache<u32, u32> {
            let mut cache = TtlSortedCache::<u32, u32>::builder()
                .ttl(Duration::from_secs(60))
                .build()
                .unwrap();
            // finite (soonest)
            cache.set_ttl(Duration::from_millis(100));
            cache.cache_set(1u32, 10u32);
            // never
            cache.set_ttl(Duration::ZERO);
            cache.cache_set(2u32, 20u32);
            // finite (later than key 1)
            cache.set_ttl(Duration::from_secs(60));
            cache.cache_set(3u32, 30u32);
            // never
            cache.set_ttl(Duration::ZERO);
            cache.cache_set(4u32, 40u32);
            cache
        }

        // count = 2: the two finite entries (1, 3) are dropped, both nevers (2, 4) kept.
        let mut cache = fresh();
        let dropped = cache.retain_latest(2, false);
        assert_eq!(dropped, 2);
        assert_eq!(cache.cache_get(&1u32), None, "soonest finite dropped");
        assert_eq!(cache.cache_get(&3u32), None, "later finite dropped");
        assert_eq!(cache.cache_get(&2u32), Some(&20u32), "never-expires kept");
        assert_eq!(cache.cache_get(&4u32), Some(&40u32), "never-expires kept");

        // count = 3: only the soonest finite (key 1) is dropped; key 3 and both nevers kept.
        let mut cache = fresh();
        let dropped = cache.retain_latest(3, false);
        assert_eq!(dropped, 1);
        assert_eq!(cache.cache_get(&1u32), None, "soonest finite dropped first");
        assert_eq!(cache.cache_get(&3u32), Some(&30u32));
        assert_eq!(cache.cache_get(&2u32), Some(&20u32));
        assert_eq!(cache.cache_get(&4u32), Some(&40u32));

        // count = 1: both finite dropped, then ONE never must be dropped. The surviving
        // entry must be a never-expires entry (key 2 or key 4), never a finite one.
        let mut cache = fresh();
        let dropped = cache.retain_latest(1, false);
        assert_eq!(dropped, 3);
        assert_eq!(cache.cache_size(), 1);
        assert_eq!(cache.cache_get(&1u32), None);
        assert_eq!(cache.cache_get(&3u32), None);
        let survivor_is_never =
            cache.cache_get(&2u32).is_some() || cache.cache_get(&4u32).is_some();
        assert!(
            survivor_is_never,
            "the last-retained entry must be a never-expires entry, not a finite one"
        );

        // count = 0: everything dropped.
        let mut cache = fresh();
        let dropped = cache.retain_latest(0, false);
        assert_eq!(dropped, 4);
        assert_eq!(cache.cache_size(), 0);
    }

    /// Max-size eviction with never-expires and finite entries interleaved in insertion
    /// order: finite entries are always evicted before never-expires entries, regardless
    /// of when the never-expires entries were inserted.
    #[test]
    fn max_size_eviction_evicts_finite_before_never_interleaved() {
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .max_size(3)
            .build()
            .unwrap();
        // Insert a never-expires entry FIRST (oldest by insertion order).
        cache.set_ttl(Duration::ZERO);
        cache.cache_set(1u32, 10u32);
        // Then finite entries.
        cache.set_ttl(Duration::from_secs(30));
        cache.cache_set(2u32, 20u32);
        cache.cache_set(3u32, 30u32);
        assert_eq!(cache.cache_size(), 3);
        // A 4th finite insert exceeds max_size=3 -> evict the soonest-expiring (a finite one).
        cache.cache_set(4u32, 40u32);
        assert_eq!(cache.cache_size(), 3);
        assert_eq!(
            cache.cache_get(&1u32),
            Some(&10u32),
            "the oldest-inserted never-expires entry must not be evicted"
        );
        // The evicted one must be a finite entry (key 2 was the soonest of the finites).
        assert_eq!(cache.cache_get(&2u32), None, "soonest finite evicted");
        // Push more finite inserts; the never-expires entry must keep surviving.
        cache.cache_set(5u32, 50u32);
        cache.cache_set(6u32, 60u32);
        assert_eq!(cache.cache_size(), 3);
        assert_eq!(
            cache.cache_get(&1u32),
            Some(&10u32),
            "never-expires entry survives repeated finite-driven eviction"
        );
    }

    /// `cache_get_or_set_with` when the cache TTL is zero: the just-inserted entry is
    /// retrievable immediately and never expires (stored with expiry = None).
    #[test]
    fn get_or_set_with_zero_ttl_inserts_never_expiring_entry() {
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_millis(10))
            .build()
            .unwrap();
        cache.set_ttl(Duration::ZERO);
        // Miss path computes and inserts; value retrievable immediately.
        let v = cache.cache_get_or_set_with(1u32, || 42u32);
        assert_eq!(*v, 42);
        assert!(
            cache
                .map
                .get(&1u32)
                .expect("entry present")
                .expiry
                .is_none(),
            "zero-ttl get_or_set must store expiry = None"
        );
        // Persists well past the former 10ms TTL.
        std::thread::sleep(std::time::Duration::from_millis(60));
        assert_eq!(
            cache.cache_get(&1u32),
            Some(&42u32),
            "zero-ttl get_or_set entry must never expire"
        );
        // Hit path: closure must not run.
        let v = cache.cache_get_or_set_with(1u32, || 999u32);
        assert_eq!(*v, 42, "existing never-expiring entry returned on hit");
    }

    /// `cache_try_get_or_set_with` when the cache TTL is zero: same contract via the
    /// fallible path. The entry is retrievable immediately and never expires.
    #[test]
    fn try_get_or_set_with_zero_ttl_inserts_never_expiring_entry() {
        let mut cache = TtlSortedCache::<&str, u32>::builder()
            .ttl(Duration::from_millis(10))
            .build()
            .unwrap();
        cache.set_ttl(Duration::ZERO);
        let v: &u32 = cache
            .cache_try_get_or_set_with("k", || Ok::<u32, ()>(7))
            .unwrap();
        assert_eq!(*v, 7);
        assert!(
            cache.map.get("k").expect("entry present").expiry.is_none(),
            "zero-ttl try_get_or_set must store expiry = None"
        );
        std::thread::sleep(std::time::Duration::from_millis(60));
        assert_eq!(
            cache.cache_get("k"),
            Some(&7u32),
            "zero-ttl try_get_or_set entry must never expire"
        );
    }

    // --- custom hasher tests ---

    #[test]
    fn custom_hasher_get_set_round_trip() {
        use crate::stores::Cached;
        use std::collections::hash_map::RandomState;
        let mut c = TtlSortedCache::<u32, u32>::builder()
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
        use crate::stores::Cached;
        let mut c: TtlSortedCache<u32, u32> = TtlSortedCache::new(Duration::from_secs(60));
        c.cache_set(1, 10);
        assert_eq!(c.cache_get(&1), Some(&10));
    }

    #[test]
    fn custom_hasher_respects_ttl_expiry() {
        use crate::stores::Cached;
        use std::collections::hash_map::RandomState;
        let mut c = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_millis(50))
            .hasher(RandomState::new())
            .build()
            .unwrap();
        c.cache_set(1, 10);
        assert_eq!(c.cache_get(&1), Some(&10));
        std::thread::sleep(std::time::Duration::from_millis(100));
        // After TTL, entry should expire (lazy removal on cache_get).
        assert_eq!(c.cache_get(&1), None, "entry must expire after ttl");
    }

    #[test]
    fn builder_initial_capacity_method_exists_and_preallocates() {
        // Verifies the renamed builder method: initial_capacity() sets a preallocation hint.
        let cache = TtlSortedCache::<u32, u32>::builder()
            .ttl_secs(60)
            .initial_capacity(32)
            .build()
            .unwrap();
        // The backing map must have at least the requested capacity.
        assert!(cache.map.capacity() >= 32);
    }

    // ── Adversarial coverage for the `set_with` builder (outside-in review) ─────

    /// `.ttl()` combined with a size cap: a shorter per-entry override moves an entry
    /// to the FRONT of the eviction order even though it was inserted after entries using
    /// the (longer) cache default TTL. This proves eviction ordering is driven by the
    /// effective (overridden) expiry, not by the cache-level default or insertion order.
    #[test]
    fn set_with_ttl_override_shorter_than_existing_evicts_first_under_size_cap() {
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .max_size(2)
            .build()
            .unwrap();

        // key 1: very short override, inserted FIRST.
        cache
            .set_with(1u32, 10u32)
            .ttl(Duration::from_millis(5))
            .set();
        // key 2: cache default (60s), inserted SECOND.
        cache.set_with(2u32, 20u32).set();
        assert_eq!(cache.cache_size(), 2, "at cap, no eviction yet");

        // key 3: also cache default. Exceeding the cap must evict the entry with the
        // soonest effective expiry — key 1 (short override) — not key 2, even though
        // key 1 was inserted earlier and key 2 uses the (longer) cache default.
        cache.set_with(3u32, 30u32).evict().set();
        assert_eq!(cache.cache_size(), 2, "size cap must still be enforced");
        assert_eq!(
            cache.cache_get(&1u32),
            None,
            "shorter .ttl() override must be evicted first despite earlier insertion"
        );
        assert_eq!(cache.cache_get(&2u32), Some(&20u32));
        assert_eq!(cache.cache_get(&3u32), Some(&30u32));
    }

    /// `.ttl(Duration::ZERO)` (never-expires) interacts with size-cap eviction ordering
    /// through the BUILDER path specifically (not `set_ttl` + plain `set`): a never-expiring
    /// entry set via `.ttl(Duration::ZERO)` must be evicted LAST, after finite entries —
    /// even a finite entry using a *shorter-than-default* override inserted later.
    #[test]
    fn set_with_ttl_zero_vs_dated_entries_eviction_order_under_size_cap() {
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .max_size(2)
            .build()
            .unwrap();

        // key 1: never-expires, via the builder's `.ttl(Duration::ZERO)`.
        cache.set_with(1u32, 10u32).ttl(Duration::ZERO).set();
        // key 2: finite, shorter than the cache default.
        cache
            .set_with(2u32, 20u32)
            .ttl(Duration::from_secs(30))
            .set();
        // key 3: finite, cache default (60s) — plain `set_with` with no `.ttl()` override.
        // This unconditionally trims to the cap (size-limit enforcement is unconditional
        // in `set_inner`), evicting the soonest-to-expire live entry: key 2.
        cache.set_with(3u32, 30u32).set();

        assert_eq!(cache.cache_size(), 2, "size cap must be enforced");
        assert_eq!(
            cache.cache_get(&1u32),
            Some(&10u32),
            "never-expiring entry set via the builder must survive size eviction"
        );
        assert_eq!(
            cache.cache_get(&2u32),
            None,
            "the shorter finite entry must be evicted before the never-expiring one"
        );
        assert_eq!(cache.cache_get(&3u32), Some(&30u32));
    }

    /// Displaced-value semantics of the terminal `.set()` across every entry state
    /// (absent, live, expired), each exercised WITH `.evict()` chained. The eviction
    /// pass must not change what `.set()` returns for the just-overwritten key — that
    /// return value reflects only the entry displaced at THIS key, computed before the
    /// (possibly unrelated) eviction sweep runs.
    #[test]
    fn set_with_evict_returns_displaced_value_in_all_states() {
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_millis(50))
            .build()
            .unwrap();

        // Absent key, `.evict()` chained: None, and the (no-op) eviction pass does not error.
        assert_eq!(cache.set_with(1u32, 10u32).evict().set(), None);

        // Live key, `.evict()` chained: displaced value returned regardless of the sweep.
        assert_eq!(cache.set_with(1u32, 11u32).evict().set(), Some(10u32));
        assert_eq!(cache.cache_get(&1u32), Some(&11u32));

        // Let it expire, then overwrite with `.evict()` chained: displaced value is
        // filtered to None (matching the non-evict path), not the stale value.
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(
            cache.set_with(1u32, 12u32).evict().set(),
            None,
            "overwriting an expired entry must return None even with .evict() chained"
        );
        assert_eq!(cache.cache_get(&1u32), Some(&12u32));
    }

    /// A just-inserted entry can itself be selected for size-limit eviction if it is the
    /// soonest-to-expire entry once the cap is exceeded — proving eviction runs strictly
    /// AFTER insertion (the code checks `map.len() > size_limit` post-insert), not before.
    /// If eviction ran before insertion, the cap would be violated (2 entries with a
    /// cap of 1). `.set()`'s displaced-value return (`None`, a new key) does not reflect
    /// this immediate self-eviction — a real gotcha for callers relying on the return value
    /// alone to know whether their write "stuck".
    #[test]
    fn set_with_evict_may_evict_the_entry_just_inserted() {
        let events = Arc::new(std::sync::Mutex::new(Vec::<(u32, u32)>::new()));
        let events2 = events.clone();
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .max_size(1)
            .on_evict(move |k: &u32, v: &u32| {
                events2.lock().unwrap().push((*k, *v));
            })
            .build()
            .unwrap();

        // Long-lived entry occupies the sole capacity slot.
        cache.set_with(1u32, 100u32).set();
        assert_eq!(cache.cache_size(), 1);
        assert_eq!(cache.cache_evictions(), Some(0));

        // Insert a NEW key with a much shorter TTL than the existing entry. Post-insert the
        // map has 2 entries against a cap of 1; the soonest-to-expire (the entry just
        // inserted) is evicted immediately — its own insertion is what triggered the check.
        let displaced = cache
            .set_with(2u32, 200u32)
            .ttl(Duration::from_millis(1))
            .evict()
            .set();
        assert_eq!(
            displaced, None,
            "the terminal .set() return reflects only same-key displacement, not self-eviction"
        );

        assert_eq!(cache.cache_size(), 1, "size cap must still be enforced");
        assert_eq!(
            cache.cache_get(&1u32),
            Some(&100u32),
            "the long-lived entry must survive"
        );
        assert_eq!(
            cache.cache_get(&2u32),
            None,
            "the just-inserted, soonest-to-expire entry must have been evicted"
        );

        // on_evict fired exactly once, for the evicted key/value — proving the callback
        // observes the entry as already inserted-and-then-removed, not skipped pre-insert.
        let fired = events.lock().unwrap().clone();
        assert_eq!(
            fired,
            vec![(2u32, 200u32)],
            "on_evict must fire exactly once, for the just-inserted key that was evicted"
        );
        assert_eq!(cache.cache_evictions(), Some(1));
    }

    /// `on_evict` fires via the builder path for a size-limit eviction with the correct
    /// (k, v) of the evicted (displaced-by-cap) entry, and the eviction counter reflects
    /// exactly one eviction — isolating the callback-content assertion (as distinct from
    /// `set_with_evict_triggers_eviction`, which only checks the counter/survivorship).
    #[test]
    fn set_with_evict_on_evict_fires_with_correct_kv_for_size_eviction() {
        let events = Arc::new(std::sync::Mutex::new(Vec::<(u32, u32)>::new()));
        let events2 = events.clone();
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .max_size(1)
            .on_evict(move |k: &u32, v: &u32| {
                events2.lock().unwrap().push((*k, *v));
            })
            .build()
            .unwrap();

        cache.set_with(1u32, 111u32).set();
        assert!(events.lock().unwrap().is_empty());

        // key 2 uses the default (longer-lived by construction order) TTL; key 1 is the
        // sole occupant and thus the only candidate to evict when the cap is exceeded.
        cache.set_with(2u32, 222u32).evict().set();

        let fired = events.lock().unwrap().clone();
        assert_eq!(
            fired,
            vec![(1u32, 111u32)],
            "on_evict must report the evicted entry's own key/value, not the new one"
        );
        assert_eq!(cache.cache_evictions(), Some(1));
        assert_eq!(cache.cache_get(&2u32), Some(&222u32));
    }

    /// Assert the `map` / `keys` lockstep invariant of `TtlSortedCache`: the expiry-ordered
    /// `BTreeSet` index holds exactly one `Stamped` per stored map entry, each rebuildable
    /// via `Entry::as_stamped`, and never a `key: None` sentinel (which `evict()` /
    /// `retain_latest()` would `expect`-panic on).
    fn assert_index_lockstep<K, V, S>(cache: &TtlSortedCache<K, V, S>, ctx: &str)
    where
        K: Hash + Eq + Ord + std::fmt::Debug,
    {
        assert_eq!(
            cache.keys.len(),
            cache.map.len(),
            "{ctx}: BTreeSet index length must equal map length"
        );
        for stamped in &cache.keys {
            assert!(
                stamped.key.is_some(),
                "{ctx}: only artificial range bounds may have a None key"
            );
        }
        for (k, entry) in &cache.map {
            assert!(
                cache.keys.contains(&entry.as_stamped()),
                "{ctx}: map entry {k:?} is missing from the BTreeSet index"
            );
        }
    }

    #[test]
    fn retain_keeps_btreeset_index_in_lockstep_with_map() {
        // A mixed population: normal TTL entries plus never-expiring ones (zero-TTL path
        // stores `expiry = None`, which sorts last in the index).
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        for k in 0u32..6 {
            cache.set(k, k * 10);
        }
        // 100/101 never expire.
        cache.set_with(100u32, 1000u32).ttl(Duration::ZERO).set();
        cache.set_with(101u32, 1010u32).ttl(Duration::ZERO).set();
        assert_index_lockstep(&cache, "before retain");

        // Drop the odd keys (including a never-expiring one) from a live population.
        cache.retain(|k, _v| k % 2 == 0);

        assert_index_lockstep(&cache, "after predicate retain");
        assert_eq!(cache.map.len(), 4, "0, 2, 4 and 100 survive, nothing else");
        assert!(cache.map.contains_key(&100u32));
        assert!(!cache.map.contains_key(&101u32));
    }

    #[test]
    fn retain_index_lockstep_when_expired_entries_are_swept() {
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_millis(20))
            .build()
            .unwrap();
        cache.set(1u32, 10u32);
        cache.set(2u32, 20u32);
        // Never expires, so it must survive the expiry sweep.
        cache.set_with(3u32, 30u32).ttl(Duration::ZERO).set();
        std::thread::sleep(std::time::Duration::from_millis(60));
        // Long-lived entry added after the sleep.
        cache
            .set_with(4u32, 40u32)
            .ttl(Duration::from_secs(60))
            .set();
        assert_index_lockstep(&cache, "before expiry retain");

        // Keep-everything predicate: only the expired entries may be removed.
        cache.retain(|_k, _v| true);

        assert_index_lockstep(&cache, "after expiry retain");
        assert_eq!(cache.map.len(), 2);
        assert!(cache.map.contains_key(&3u32), "never-expiring entry stays");
        assert!(cache.map.contains_key(&4u32), "live entry stays");

        // A stale index entry would make `pop_first` count a drop for a key that is no
        // longer in the map; the swept entries must not be counted again here.
        assert_eq!(cache.evict(), 0, "no stale index entries left to sweep");
        assert_index_lockstep(&cache, "after post-retain evict");
    }

    /// `retain` deliberately ignores `size_limit`, so the cap is restored by the *next*
    /// insert. That insert goes through `retain_latest`, which pops the expiry index: a
    /// stale stamp left behind by `retain` would be popped first, counted as a drop even
    /// though its map entry is gone, and leave the cache over its cap.
    #[test]
    fn retain_index_lockstep_when_a_size_limited_insert_follows() {
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .max_size(3)
            .build()
            .unwrap();
        for k in 1u32..=3 {
            cache
                .set_with(k, k * 10)
                .ttl(Duration::from_secs(10 * u64::from(k)))
                .set();
        }
        assert_index_lockstep(&cache, "before retain");

        // Drop the soonest-to-expire entry -- the one a stale stamp would make the
        // phantom victim of the next size-limit eviction.
        cache.retain(|k, _v| *k != 1);
        assert_index_lockstep(&cache, "after retain");
        assert_eq!(cache.map.len(), 2, "retain does not enforce size_limit");

        // Back to the cap exactly: still no eviction (the check is `len > size_limit`).
        cache
            .set_with(4u32, 40u32)
            .ttl(Duration::from_secs(40))
            .set();
        assert_eq!(cache.map.len(), 3);
        assert_index_lockstep(&cache, "at the cap");

        // Over the cap: the victim must be key 2, the soonest-to-expire *survivor*.
        cache
            .set_with(5u32, 50u32)
            .ttl(Duration::from_secs(50))
            .set();
        assert_eq!(cache.map.len(), 3, "the cap is restored by the insert");
        assert!(
            !cache.map.contains_key(&2u32),
            "the soonest-to-expire survivor is the victim"
        );
        assert!(cache.map.contains_key(&3u32));
        assert!(cache.map.contains_key(&4u32));
        assert!(cache.map.contains_key(&5u32));
        assert_index_lockstep(&cache, "after size-limit eviction");
    }

    /// `set_and_get_mut` temporarily unlinks the just-inserted `Stamped` from the index
    /// before running `retain_latest`, then relinks it. Interleaving that protected
    /// eviction path with `retain` must leave the index in lockstep and evict the correct
    /// victim.
    #[test]
    fn retain_index_lockstep_with_protected_get_or_set_with_mut() {
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .max_size(2)
            .build()
            .unwrap();
        cache
            .set_with(1u32, 10u32)
            .ttl(Duration::from_secs(10))
            .set();
        cache
            .set_with(2u32, 20u32)
            .ttl(Duration::from_secs(20))
            .set();
        cache.retain(|k, _v| *k != 1);
        assert_index_lockstep(&cache, "after retain");

        // Below the cap: the protected-eviction block is skipped entirely.
        assert_eq!(*cache.cache_get_or_set_with_mut(3u32, || 30), 30);
        assert_index_lockstep(&cache, "after unprotected insert");

        // Over the cap: the just-inserted key 4 is unlinked, so key 2 (soonest to
        // expire of the rest) is the victim, and key 4's stamp must be relinked.
        {
            let v = cache.cache_get_or_set_with_mut(4u32, || 40);
            *v += 1;
        }
        assert_eq!(cache.map.len(), 2);
        assert!(cache.map.contains_key(&4u32), "the new entry is protected");
        assert!(!cache.map.contains_key(&2u32));
        assert_index_lockstep(&cache, "after protected eviction");

        // The relinked stamp must be usable by a later index-driven trim.
        assert_eq!(cache.retain_latest(0, false), 2);
        assert!(cache.map.is_empty());
        assert_index_lockstep(&cache, "after full trim");
    }

    /// Same protected path, reached through the fallible factory. An `Err` factory must
    /// not touch either structure.
    #[test]
    fn retain_index_lockstep_with_protected_try_get_or_set_with_mut() {
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .max_size(2)
            .build()
            .unwrap();
        cache
            .set_with(1u32, 10u32)
            .ttl(Duration::from_secs(10))
            .set();
        cache
            .set_with(2u32, 20u32)
            .ttl(Duration::from_secs(20))
            .set();
        cache.retain(|k, _v| *k != 1);

        let err: Result<&mut u32, &str> = cache.cache_try_get_or_set_with_mut(9u32, || Err("no"));
        assert_eq!(err, Err("no"));
        assert_eq!(cache.map.len(), 1, "a failed factory inserts nothing");
        assert_index_lockstep(&cache, "after failed factory");

        let ok: Result<&mut u32, &str> = cache.cache_try_get_or_set_with_mut(3u32, || Ok(30));
        assert_eq!(ok, Ok(&mut 30));
        let ok: Result<&mut u32, &str> = cache.cache_try_get_or_set_with_mut(4u32, || Ok(40));
        assert_eq!(ok, Ok(&mut 40));
        assert_eq!(cache.map.len(), 2);
        assert!(cache.map.contains_key(&4u32));
        assert!(!cache.map.contains_key(&2u32), "key 2 expires soonest");
        assert_index_lockstep(&cache, "after protected eviction");
    }

    /// `Clone` copies `map` and `keys` separately, so a lockstep violation introduced by
    /// `retain` would be duplicated silently into the clone.
    #[test]
    fn retain_index_lockstep_survives_clone() {
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        for k in 0u32..6 {
            cache
                .set_with(k, k * 10)
                .ttl(Duration::from_secs(10 + u64::from(k)))
                .set();
        }
        cache.set_with(100u32, 1000u32).ttl(Duration::ZERO).set();
        cache.retain(|k, _v| k % 2 == 0);

        let mut clone = cache.clone();
        assert_index_lockstep(&clone, "clone of a retained cache");
        assert_eq!(clone.map.len(), cache.map.len());

        // The clone's index must drive its own trims correctly: 0, 2, 4 and the
        // never-expiring 100 survived, so trimming to 1 drops exactly three.
        assert_eq!(clone.retain_latest(1, false), 3);
        assert!(
            clone.map.contains_key(&100u32),
            "the never-expiring entry sorts last and is kept"
        );
        assert_index_lockstep(&clone, "clone after trim");
        // The original is untouched by the clone's trim.
        assert_eq!(cache.map.len(), 4);
        assert_index_lockstep(&cache, "original after clone trim");

        // A retain on the clone must not disturb the original either.
        clone.retain(|_k, _v| false);
        assert!(clone.map.is_empty());
        assert_eq!(cache.map.len(), 4);
        assert_index_lockstep(&cache, "original after clone retain");
    }

    /// A predicate that rejects everything must drain both structures to zero, leaving a
    /// reusable cache (not one whose index still holds phantom stamps).
    #[test]
    fn retain_rejecting_everything_drains_both_structures() {
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        for k in 0u32..4 {
            cache.set(k, k);
        }
        cache.set_with(100u32, 100u32).ttl(Duration::ZERO).set();

        cache.retain(|_k, _v| false);
        assert!(cache.map.is_empty());
        assert!(
            cache.keys.is_empty(),
            "the expiry index must be drained with the map"
        );
        assert_index_lockstep(&cache, "after draining retain");
        assert_eq!(cache.evict(), 0);
        assert_eq!(cache.retain_latest(0, true), 0);

        // Retaining an empty cache is a no-op under either predicate.
        cache.retain(|_k, _v| false);
        cache.retain(|_k, _v| true);
        assert_index_lockstep(&cache, "after empty-cache retain");

        // And the cache still works.
        cache.set(7u32, 77u32);
        assert_index_lockstep(&cache, "after reinsert");
        assert_eq!(cache.cache_get(&7u32), Some(&77u32));
    }

    /// A panicking `on_evict` must not desynchronize the map from the expiry index.
    /// `retain` therefore drains both structures before firing any callback: unwinding
    /// out of a callback that ran mid-pass would strand entries in `map` with no
    /// matching `keys` stamp, making them unreachable to `evict` / `retain_latest`
    /// (whose `pop_first` walk never sees them) while `len` still counts them.
    #[test]
    fn retain_on_evict_panic_leaves_the_index_in_lockstep() {
        // Panic on the first callback only, so the later assertions can still run
        // operations that fire `on_evict`.
        let fired = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let cb = std::sync::Arc::clone(&fired);
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .on_evict(move |_k, _v| {
                if cb.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 0 {
                    panic!("boom");
                }
            })
            .build()
            .unwrap();
        for k in 0u32..6 {
            cache.set(k, k);
        }

        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            cache.retain(|k, _v| k % 2 == 0);
        }));
        assert!(caught.is_err(), "the callback panic must propagate");

        // Both structures completed their removals before the callback ran.
        assert_index_lockstep(&cache, "after on_evict panicked mid-retain");
        assert_eq!(cache.map.len(), 3, "the odd keys were removed");
        // The survivors are still reachable through the expiry index, so a subsequent
        // trim sees all of them rather than stopping short at an orphaned entry.
        assert_eq!(cache.retain_latest(0, false), 3);
        assert_index_lockstep(&cache, "after trimming the survivors");
    }

    /// Entries inserted back to back can share an expiry `Instant` on a coarse clock, so
    /// their `Stamped`s differ only by key. `retain` must remove exactly the index entry
    /// belonging to each removed map entry, never a same-expiry neighbour.
    #[test]
    fn retain_index_lockstep_with_colliding_expiry_instants() {
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        for k in 0u32..64 {
            cache.set(k, k);
        }
        assert_index_lockstep(&cache, "before retain");

        cache.retain(|k, _v| k % 2 == 0);
        assert_eq!(cache.map.len(), 32);
        assert_index_lockstep(&cache, "after retain");
        for k in 0u32..64 {
            assert_eq!(
                cache.map.contains_key(&k),
                k % 2 == 0,
                "exactly the even keys survive"
            );
        }
        assert_eq!(cache.evict(), 0, "nothing is expired, nothing is stale");

        // Every surviving stamp must still resolve to a map entry.
        assert_eq!(cache.retain_latest(0, false), 32);
        assert!(cache.map.is_empty());
        assert!(cache.keys.is_empty());
    }

    // ---------------------------------------------------------------------------
    // Front-driven `evict_at` / `retain_latest_at` equivalence with the old two-pass
    // (`range(..).count()` then pop) logic. The real monotonic clock never produces a
    // deterministic `now == expiry` tie, so these drive the private `*_at` entry points
    // with hand-built entries and an explicit cutoff.
    // ---------------------------------------------------------------------------

    /// Insert an entry with an exact `expiry`, keeping map and index in lockstep the same
    /// way `set_inner` does (same `CacheArc` in both structures).
    fn insert_raw(
        cache: &mut TtlSortedCache<u32, u32>,
        key: u32,
        value: u32,
        expiry: Option<crate::time::Instant>,
    ) {
        use super::{CacheArc, Entry, Stamped};
        let arc = CacheArc::new(key);
        cache.keys.insert(Stamped {
            expiry,
            key: Some(arc.clone()),
        });
        cache.map.insert(
            key,
            Entry {
                expiry,
                key: arc,
                value,
            },
        );
    }

    /// The number of entries the pre-refactor sweep would have removed: the count of the
    /// `[bound(min_instant), bound(cutoff))` range over the expiry index.
    fn old_range_expired_count(
        cache: &TtlSortedCache<u32, u32>,
        cutoff: crate::time::Instant,
    ) -> usize {
        use super::Stamped;
        use std::ops::Bound::{Excluded, Included};
        let min = Stamped::<u32>::bound(cache.min_instant);
        let max = Stamped::<u32>::bound(cutoff);
        cache.keys.range((Included(&min), Excluded(&max))).count()
    }

    /// Build a cache holding one entry strictly before the cutoff, one tied exactly at the
    /// cutoff, one after it, and one that never expires. All expiries are in the future
    /// relative to `min_instant`, so the old lower range bound would not have excluded any.
    fn boundary_population() -> (TtlSortedCache<u32, u32>, crate::time::Instant) {
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        let cutoff = crate::time::Instant::now() + Duration::from_secs(1);
        insert_raw(&mut cache, 1, 10, Some(cutoff - Duration::from_nanos(1)));
        insert_raw(&mut cache, 2, 20, Some(cutoff));
        insert_raw(&mut cache, 3, 30, Some(cutoff + Duration::from_nanos(1)));
        insert_raw(&mut cache, 4, 40, None);
        (cache, cutoff)
    }

    /// An entry expiring EXACTLY at the cutoff is not swept (the old
    /// `Excluded(bound(cutoff))` bound excluded it too, because the sentinel's `key: None`
    /// sorts below every real key at the same expiry), and a never-expiring entry survives.
    #[test]
    fn evict_at_boundary_tie_and_never_expiring_entries_match_the_old_range_bounds() {
        let (mut cache, cutoff) = boundary_population();
        assert_eq!(
            old_range_expired_count(&cache, cutoff),
            1,
            "reference: the old range selected only the strictly-earlier entry"
        );

        assert_eq!(
            cache.evict_at(cutoff),
            1,
            "only the entry expiring strictly before the cutoff is swept"
        );
        assert!(!cache.map.contains_key(&1u32), "strictly earlier is swept");
        assert!(
            cache.map.contains_key(&2u32),
            "an exact now == expiry tie survives, matching the old Excluded upper bound"
        );
        assert!(cache.map.contains_key(&3u32), "later expiry survives");
        assert!(cache.map.contains_key(&4u32), "never-expiring survives");
        assert_index_lockstep(&cache, "after a boundary evict_at");

        // A cutoff one tick past the tie now sweeps it, proving the boundary is where the
        // old range put it and not one entry off.
        assert_eq!(cache.evict_at(cutoff + Duration::from_nanos(1)), 1);
        assert!(!cache.map.contains_key(&2u32));
        assert!(cache.map.contains_key(&3u32));
        assert!(cache.map.contains_key(&4u32));
        assert_index_lockstep(&cache, "after sweeping the tie");
    }

    /// Over a mixed population (many expired, some live, some never-expiring, colliding
    /// expiries) the front-driven sweep must drop exactly as many entries as the old
    /// `range(..).count()` pass would have.
    #[test]
    fn evict_at_drop_count_matches_the_old_two_pass_range_count() {
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        let cutoff = crate::time::Instant::now() + Duration::from_secs(1);
        for k in 0u32..30 {
            let expiry = match k % 3 {
                // Expired, with pairs of keys colliding on the same instant.
                0 => Some(cutoff - Duration::from_micros(u64::from(k / 3) + 1)),
                // Live: half of them tie EXACTLY on the cutoff, which must be kept.
                1 => Some(cutoff + Duration::from_micros(u64::from(k % 2))),
                // Never expires.
                _ => None,
            };
            insert_raw(&mut cache, k, k * 10, expiry);
        }
        let expected = old_range_expired_count(&cache, cutoff);
        assert_eq!(expected, 10, "reference count over the mixed population");

        assert_eq!(
            cache.evict_at(cutoff),
            expected,
            "front-driven evict must drop the same count as the old range pass"
        );
        assert_eq!(cache.map.len(), 20);
        assert_index_lockstep(&cache, "after a mixed evict_at");
        for k in 0u32..30 {
            assert_eq!(
                cache.map.contains_key(&k),
                k % 3 != 0,
                "exactly the expired third is swept"
            );
        }
        assert_eq!(
            cache.evict_at(cutoff),
            0,
            "a repeat sweep at the same cutoff drops nothing"
        );
    }

    /// A panicking `on_evict` must not desynchronize the map from the expiry index during an
    /// expiry sweep either. `evict` detaches the whole expired prefix from `keys` up front, so
    /// it must finish every `map` removal before firing any callback — unwinding mid-drain
    /// would strand the not-yet-removed entries in `map` with their stamps already gone.
    #[test]
    fn evict_on_evict_panic_leaves_the_index_in_lockstep() {
        let fired = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let cb = std::sync::Arc::clone(&fired);
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .on_evict(move |_k, _v| {
                // Panic on the FIRST callback: with an interleaved drain that would leave the
                // remaining five entries in `map` without stamps.
                if cb.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 0 {
                    panic!("boom");
                }
            })
            .build()
            .unwrap();
        let cutoff = crate::time::Instant::now() + Duration::from_secs(1);
        for k in 0u32..6 {
            insert_raw(
                &mut cache,
                k,
                k,
                Some(cutoff - Duration::from_micros(u64::from(k) + 1)),
            );
        }
        // One live entry that must survive untouched.
        insert_raw(&mut cache, 100u32, 100u32, Some(cutoff));

        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = cache.evict_at(cutoff);
        }));
        assert!(caught.is_err(), "the callback panic must propagate");

        assert_index_lockstep(&cache, "after on_evict panicked mid-evict");
        assert_eq!(
            cache.map.len(),
            1,
            "every expired entry left the map before the callbacks ran"
        );
        assert!(cache.map.contains_key(&100u32), "the live entry survives");
        assert_eq!(cache.cache_evictions(), Some(6));
        // The survivor is still reachable through the index.
        assert_eq!(cache.retain_latest(0, false), 1);
        assert_index_lockstep(&cache, "after trimming the survivor");
    }

    /// A panicking `on_evict` must not desynchronize the map from the expiry index during a
    /// `retain_latest` size trim either. Unlike `evict_at`'s batch-then-notify drain,
    /// `retain_latest_at`'s pop loop fires `on_evict` inline per popped entry, so this path
    /// only stays correct because each iteration finishes its `map` removal and the eviction
    /// counter bump BEFORE the callback runs. Panic on the third callback (after two full,
    /// uninterrupted iterations) so the test also proves the ordering holds across repeated
    /// iterations, not merely on the very first one.
    #[test]
    fn retain_latest_on_evict_panic_leaves_the_index_in_lockstep() {
        let fired = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let cb = std::sync::Arc::clone(&fired);
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .on_evict(move |_k, _v| {
                if cb.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 2 {
                    panic!("boom");
                }
            })
            .build()
            .unwrap();
        let cutoff = crate::time::Instant::now() + Duration::from_secs(60);
        // Five entries with strictly ascending expiry (soonest-to-expire first, matching the
        // order `retain_latest_at`'s pop loop visits them in), plus a never-expiring survivor
        // that must remain untouched by the trim.
        for k in 0u32..5 {
            insert_raw(
                &mut cache,
                k,
                k,
                Some(cutoff - Duration::from_secs(5) + Duration::from_micros(u64::from(k))),
            );
        }
        insert_raw(&mut cache, 100u32, 100u32, None);

        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // Keep only 1 entry: this must trim k=0..4, dropping the far end of the pop loop
            // to the third callback's panic.
            let _ = cache.retain_latest_at(1, None);
        }));
        assert!(caught.is_err(), "the callback panic must propagate");

        assert_index_lockstep(&cache, "after on_evict panicked mid retain_latest_at");
        // k=0 and k=1 completed a full (pop, map.remove, counter bump, callback) cycle before
        // the panic; k=2's own removal also completed before its callback panicked. A reordered
        // `retain_latest_at` that fires the callback before removing from `map` would strand
        // k=2 in `map` with its stamp already gone from `keys` -- caught by the lockstep assert
        // above, but pinned down explicitly here too.
        assert!(!cache.map.contains_key(&0u32), "k=0 fully removed");
        assert!(!cache.map.contains_key(&1u32), "k=1 fully removed");
        assert!(
            !cache.map.contains_key(&2u32),
            "k=2 fully removed despite its callback panic"
        );
        // The pop loop never reached these: it unwound out of the panicking callback first.
        assert!(
            cache.map.contains_key(&3u32),
            "k=3 untouched by the interrupted trim"
        );
        assert!(
            cache.map.contains_key(&4u32),
            "k=4 untouched by the interrupted trim"
        );
        assert!(
            cache.map.contains_key(&100u32),
            "never-expiring survivor untouched"
        );
        assert_eq!(
            cache.cache_evictions(),
            Some(3),
            "counter reflects exactly the three rows removed before the panic"
        );
        // The survivor is still reachable through the index after the panic.
        assert_eq!(cache.cache_get(&100u32), Some(&100u32));
        assert_eq!(cache.retain_latest(0, false), 3);
        assert_index_lockstep(&cache, "after trimming what the panic left behind");
    }

    /// The value-`Drop` panic path of the `retain_latest_at` size trim, with no `on_evict`
    /// configured. Unlike `evict_at` -- which detaches the whole expired prefix from
    /// `self.keys` in one `split_off` and so must drain `self.map` into a local `Vec` before
    /// any value drops -- `retain_latest_at`'s pop loop removes one stamp and its map row
    /// together before that value drops, so a panicking `Drop` mid-trim leaves map and keys
    /// in lockstep without a collect step. A regression that detached the trim prefix up front
    /// and dropped per iteration (as the pre-fix `evict_at` did) would orphan the
    /// not-yet-removed rows: their stamps already gone from `self.keys` while their entries
    /// linger in `self.map`.
    #[test]
    fn retain_latest_no_callback_keeps_map_and_keys_in_lockstep_on_drop_panic() {
        use std::panic::{AssertUnwindSafe, catch_unwind};

        // A value whose `Drop` panics only when armed. Exactly ONE instance is armed so the
        // unwind never double-panics into a process abort.
        struct PanicOnDrop {
            armed: bool,
        }
        impl Drop for PanicOnDrop {
            fn drop(&mut self) {
                if self.armed {
                    panic!("PanicOnDrop::drop fired");
                }
            }
        }

        // No `on_evict` and no `size_limit`: the trim runs through `retain_latest` directly.
        let mut cache: TtlSortedCache<u32, PanicOnDrop> = TtlSortedCache::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        // Five finite entries with ascending expiry (soonest first, the pop order). Arm the
        // THIRD to pop (key 2) so two full iterations complete before it drops -- proving the
        // remove-before-drop ordering holds across iterations, and leaving keys 3/4 to be
        // stranded by a batched regression.
        for k in 0u32..5 {
            cache
                .set_with(k, PanicOnDrop { armed: k == 2 })
                .ttl(Duration::from_secs(10 * (u64::from(k) + 1)))
                .set();
        }
        assert_eq!(cache.map.len(), 5);
        assert_eq!(cache.keys.len(), 5);

        // Keep 1 -> retain_drop_count = 4: the trim pops keys 0..=3, and the armed value's
        // Drop panics on the third pop before keys 3/4 are reached.
        let result = catch_unwind(AssertUnwindSafe(|| {
            let _ = cache.retain_latest(1, false);
        }));
        assert!(
            result.is_err(),
            "the armed value's Drop must have panicked mid-trim"
        );

        // Lockstep is the core property: whatever left `self.keys` also left `self.map`.
        assert_index_lockstep(&cache, "after a Drop panic mid retain_latest trim");
        assert!(!cache.map.contains_key(&0u32), "key 0 fully removed");
        assert!(!cache.map.contains_key(&1u32), "key 1 fully removed");
        assert!(
            !cache.map.contains_key(&2u32),
            "the armed row left the map before its Drop panicked"
        );
        assert!(
            cache.map.contains_key(&3u32),
            "key 3 untouched by the interrupted trim"
        );
        assert!(
            cache.map.contains_key(&4u32),
            "key 4 untouched by the interrupted trim"
        );
        assert_eq!(cache.map.len(), 2);
        // The counter is bumped before each value drops, so all three removed rows are counted.
        assert_eq!(
            cache.cache_evictions(),
            Some(3),
            "the counter reflects the three rows removed before the panic"
        );
        // The survivors remain reachable through the index for a later trim.
        assert_eq!(cache.retain_latest(0, false), 2);
        assert!(cache.map.is_empty());
        assert!(cache.keys.is_empty());
    }

    /// `retain_latest_at` with the sweep enabled: the tie at the cutoff is kept and
    /// never-expiring entries sort last, exactly as under the old range bounds.
    #[test]
    fn retain_latest_at_boundary_tie_and_never_expiring_entries_match_the_old_range_bounds() {
        // count = 4 (no size pressure): only the expiry sweep can drop anything.
        let (mut cache, cutoff) = boundary_population();
        assert_eq!(
            cache.retain_latest_at(4, Some(cutoff)),
            1,
            "only the strictly-earlier entry is swept; the tie is kept"
        );
        assert!(cache.map.contains_key(&2u32), "the exact tie survives");
        assert!(cache.map.contains_key(&4u32), "never-expiring survives");
        assert_index_lockstep(&cache, "after a boundary retain_latest_at");

        // Size pressure beyond the expired prefix: the sweep count and the trim count are
        // combined exactly as `max(retain_drop_count, expired_count)` did.
        let (mut cache, cutoff) = boundary_population();
        assert_eq!(
            cache.retain_latest_at(2, Some(cutoff)),
            2,
            "trim of 2 dominates the single expired entry"
        );
        assert!(!cache.map.contains_key(&1u32), "expired dropped first");
        assert!(!cache.map.contains_key(&2u32), "then the soonest-to-expire");
        assert!(
            cache.map.contains_key(&4u32),
            "never-expiring entries are retained last"
        );
        assert_index_lockstep(&cache, "after a size-dominated retain_latest_at");

        // Sweep disabled: a size trim alone never consults the cutoff.
        let (mut cache, _cutoff) = boundary_population();
        assert_eq!(cache.retain_latest_at(4, None), 0);
        assert_eq!(cache.map.len(), 4, "no trim needed, no sweep requested");
        assert_index_lockstep(&cache, "after a no-op retain_latest_at");
    }

    /// The combined trim/sweep drop count must equal the old
    /// `max(retain_drop_count, range(..).count())` for every `count` and both sweep modes.
    #[test]
    fn retain_latest_at_drop_count_matches_the_old_two_pass_logic() {
        fn fresh() -> (TtlSortedCache<u32, u32>, crate::time::Instant) {
            let mut cache = TtlSortedCache::<u32, u32>::builder()
                .ttl(Duration::from_secs(60))
                .build()
                .unwrap();
            let cutoff = crate::time::Instant::now() + Duration::from_secs(1);
            for k in 0u32..12 {
                let expiry = match k % 3 {
                    0 => Some(cutoff - Duration::from_micros(u64::from(k) + 1)),
                    // Half of the live entries tie EXACTLY on the cutoff.
                    1 => Some(cutoff + Duration::from_micros(u64::from(k % 2))),
                    _ => None,
                };
                insert_raw(&mut cache, k, k * 10, expiry);
            }
            (cache, cutoff)
        }

        for keep in 0..=13usize {
            for sweep in [false, true] {
                let (mut cache, cutoff) = fresh();
                let retain_drop_count = cache.map.len().saturating_sub(keep);
                let expected = if sweep {
                    retain_drop_count.max(old_range_expired_count(&cache, cutoff))
                } else {
                    retain_drop_count
                };
                let dropped = cache.retain_latest_at(keep, sweep.then_some(cutoff));
                assert_eq!(
                    dropped, expected,
                    "keep={keep} sweep={sweep}: drop count must match the old two-pass logic"
                );
                assert_eq!(cache.map.len(), 12 - expected);
                assert_index_lockstep(&cache, "after retain_latest_at");
            }
        }
    }

    // ---------------------------------------------------------------------------
    // `set_inner` / `set_and_get_mut` refactor
    // ---------------------------------------------------------------------------

    /// `set_and_get_mut` no longer re-looks-up the key after inserting; it reuses the stamp
    /// `set_inner` returns. The map/index lockstep must hold on every branch it takes:
    /// plain miss, expired displacement, and the protected size-limited eviction.
    #[test]
    fn set_and_get_mut_keeps_the_index_in_lockstep() {
        // Plain miss, no size limit.
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        for k in 0u32..4 {
            let v = cache.cache_get_or_set_with_mut(k, || k * 10);
            assert_eq!(*v, k * 10);
            assert_index_lockstep(&cache, "after a miss insert");
        }
        assert_eq!(cache.map.len(), 4);

        // Expired displacement: the factory runs again and the stale stamp is replaced.
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_millis(20))
            .build()
            .unwrap();
        cache.cache_set(1u32, 10u32);
        std::thread::sleep(std::time::Duration::from_millis(60));
        assert_eq!(*cache.cache_get_or_set_with_mut(1u32, || 99u32), 99u32);
        assert_eq!(cache.map.len(), 1);
        assert_index_lockstep(&cache, "after replacing an expired entry");

        // Size-limited: the just-inserted entry is protected, its stamp is restored, and the
        // returned reference points at that entry.
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .max_size(3)
            .build()
            .unwrap();
        for k in 0u32..8 {
            let v = cache.cache_get_or_set_with_mut(k, || k * 100);
            assert_eq!(*v, k * 100, "the reference must be the entry just inserted");
            *v += 1;
            assert!(cache.map.len() <= 3, "the size limit is enforced");
            assert_index_lockstep(&cache, "after a protected size-limited insert");
            assert_eq!(
                crate::CachedPeek::cache_peek(&cache, &k),
                Some(&(k * 100 + 1)),
                "the protected entry survives its own insert and is mutable through the ref"
            );
        }
        // The index is still complete enough to trim everything.
        let len = cache.map.len();
        assert_eq!(cache.retain_latest(0, false), len);
        assert!(cache.keys.is_empty());
    }

    /// Overwriting an existing key reuses the stored key `Arc` (a refcount bump) instead of
    /// allocating a fresh `Arc` from a deep key clone, and the index keeps exactly one stamp.
    #[test]
    fn cache_set_overwrite_reuses_the_stored_key_arc() {
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        cache.cache_set(1u32, 10u32);
        let before = cache.map.get(&1u32).expect("entry present").key.0.clone();

        assert_eq!(cache.cache_set(1u32, 20u32), Some(10u32));
        let after = &cache.map.get(&1u32).expect("entry present").key.0;
        assert!(
            Arc::ptr_eq(&before, after),
            "an overwrite must reuse the stored key Arc, not allocate a new one"
        );
        assert_eq!(cache.map.len(), 1);
        assert_eq!(cache.keys.len(), 1, "no orphan stamp is left behind");
        assert_index_lockstep(&cache, "after an overwrite");
    }

    /// An overwrite that changes the expiry must remove the old stamp and index the new one;
    /// an overwrite that leaves the expiry unchanged must leave a single stamp in place.
    #[test]
    fn overwrite_replaces_the_index_stamp_only_when_the_expiry_changes() {
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        // Two entries so a stale stamp would be visible as a length mismatch.
        cache.set_with(1u32, 10u32).ttl(Duration::ZERO).set();
        cache.set_with(2u32, 20u32).ttl(Duration::ZERO).set();
        assert_index_lockstep(&cache, "never-expiring pair");

        // Same (None) expiry: the stamp is identical, nothing to remove.
        assert_eq!(
            cache.set_with(1u32, 11u32).ttl(Duration::ZERO).set(),
            Some(10u32)
        );
        assert_eq!(cache.keys.len(), 2);
        assert_index_lockstep(&cache, "after a same-expiry overwrite");

        // Changed expiry (None -> Some): the never-expires stamp must be dropped, leaving the
        // entry sorted by its new finite expiry (so it now evicts before the never-expiring one).
        assert_eq!(
            cache
                .set_with(1u32, 12u32)
                .ttl(Duration::from_secs(30))
                .set(),
            Some(11u32)
        );
        assert_eq!(cache.keys.len(), 2, "the stale never-expires stamp is gone");
        assert_index_lockstep(&cache, "after an expiry-changing overwrite");
        assert_eq!(cache.retain_latest(1, false), 1);
        assert!(
            !cache.map.contains_key(&1u32),
            "the re-stamped entry now expires first and is trimmed first"
        );
        assert!(cache.map.contains_key(&2u32));
        assert_index_lockstep(&cache, "after trimming the re-stamped entry");
    }

    /// The async get-or-set now checks liveness in place instead of routing through
    /// `cache_get` (which swept the expired entry before the factory even ran). A factory
    /// future dropped before completion must therefore leave the expired entry alone and
    /// must not fire `on_evict`, matching the sync path and `TtlCache`.
    #[cfg(feature = "async")]
    #[tokio::test]
    async fn async_cache_get_or_set_with_mut_cancel_keeps_expired_entry() {
        use crate::CachedGetOrSetAsync;
        use std::task::Poll;

        let fired = Arc::new(AtomicUsize::new(0));
        let fired2 = fired.clone();
        let mut c: TtlSortedCache<u32, u32> = TtlSortedCache::builder()
            .ttl(Duration::from_millis(20))
            .on_evict(move |_k: &u32, _v: &u32| {
                fired2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();
        c.cache_set(1u32, 100u32);
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;

        {
            let mut fut = Box::pin(CachedGetOrSetAsync::async_cache_get_or_set_with_mut(
                &mut c,
                1u32,
                std::future::pending::<u32>,
            ));
            let waker = std::task::Waker::noop();
            let mut cx = std::task::Context::from_waker(waker);
            assert!(
                matches!(
                    std::future::Future::poll(fut.as_mut(), &mut cx),
                    Poll::Pending
                ),
                "future must be pending while the factory is unresolved"
            );
            // Dropped here: cancellation mid-factory.
        }

        assert_eq!(
            fired.load(Ordering::Relaxed),
            0,
            "on_evict must not fire when the factory future is dropped"
        );
        assert_eq!(c.cache_evictions(), Some(0));
        assert_eq!(c.cache_size(), 1, "the expired entry must still be present");
        assert_index_lockstep(&c, "after async factory cancellation");

        // A completed factory replaces it, firing on_evict exactly once for the displaced
        // expired value.
        let v =
            CachedGetOrSetAsync::async_cache_get_or_set_with_mut(&mut c, 1u32, || async { 200u32 })
                .await;
        assert_eq!(*v, 200u32);
        assert_eq!(fired.load(Ordering::Relaxed), 1);
        assert_eq!(c.cache_evictions(), Some(1));
        assert_index_lockstep(&c, "after async replacement");
    }

    /// An async hit counts exactly one hit (and no miss), the same as the sync path: the
    /// in-place liveness check replaced a `cache_get` call that also touched the counters.
    #[cfg(feature = "async")]
    #[tokio::test]
    async fn async_cache_get_or_set_with_mut_hit_counts_one_hit() {
        use crate::CachedGetOrSetAsync;

        let mut c: TtlSortedCache<u32, u32> = TtlSortedCache::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        c.cache_set(1u32, 100u32);
        c.cache_reset_metrics();

        let v = CachedGetOrSetAsync::async_cache_get_or_set_with_mut(&mut c, 1u32, || async {
            unreachable!("the factory must not run on a live hit")
        })
        .await;
        assert_eq!(*v, 100u32);
        assert_eq!(c.cache_hits(), Some(1));
        assert_eq!(c.cache_misses(), Some(0));

        // And a miss counts exactly one miss.
        let v =
            CachedGetOrSetAsync::async_cache_get_or_set_with_mut(&mut c, 2u32, || async { 200u32 })
                .await;
        assert_eq!(*v, 200u32);
        assert_eq!(c.cache_hits(), Some(1));
        assert_eq!(c.cache_misses(), Some(1));
        assert_index_lockstep(&c, "after an async miss insert");
    }

    /// The fallible async sibling (`async_cache_try_get_or_set_with_mut`) was never given a
    /// dedicated absent-key test: the flagged risk is that its explicit `hits`/`misses`
    /// increments (replacing the old implicit `cache_get`-driven ones) could be off by one or
    /// double-counted on the paths the big cross-variant table test does not visit (a key that
    /// was never inserted, as opposed to one that expired). Walks a live hit, an absent-key
    /// `Err`, an absent-key `Ok`, an expired-key `Err`, and an expired-key `Ok` in sequence,
    /// asserting the exact running `hits`/`misses` totals and `on_evict`/`evictions` at each
    /// step -- a double count or a missed count on any single path desyncs the running total
    /// for every step after it.
    #[cfg(feature = "async")]
    #[tokio::test]
    async fn async_try_get_or_set_with_mut_hits_and_misses_match_across_absent_expired_and_live() {
        use crate::CachedGetOrSetAsync;

        let fired = Arc::new(AtomicUsize::new(0));
        let fired2 = fired.clone();
        let mut c: TtlSortedCache<u32, u32> = TtlSortedCache::builder()
            .ttl(Duration::from_millis(20))
            .on_evict(move |_k: &u32, _v: &u32| {
                fired2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();

        // A live entry (key 1, long TTL via the builder) to hit against.
        c.set_with(1u32, 100u32).ttl(Duration::from_secs(60)).set();
        c.cache_reset_metrics();

        // Step 1: LIVE HIT. The factory must not run; exactly one hit, no miss, nothing
        // evicted, the value is unchanged.
        let v: Result<&mut u32, &'static str> =
            CachedGetOrSetAsync::async_cache_try_get_or_set_with_mut(&mut c, 1u32, || async {
                unreachable!("the factory must not run on a live hit")
            })
            .await;
        assert_eq!(v.ok(), Some(&mut 100u32));
        assert_eq!(c.cache_hits(), Some(1), "step 1: one hit");
        assert_eq!(c.cache_misses(), Some(0), "step 1: no miss");
        assert_eq!(fired.load(Ordering::Relaxed), 0);
        assert_eq!(c.cache_evictions(), Some(0));

        // Step 2: ABSENT KEY, factory Err. Nothing is stored, one miss is counted (before the
        // factory runs), no eviction fires (there was nothing to displace).
        let v: Result<&mut u32, &'static str> =
            CachedGetOrSetAsync::async_cache_try_get_or_set_with_mut(&mut c, 2u32, || async {
                Err("nope")
            })
            .await;
        assert_eq!(v.err(), Some("nope"));
        // `cache_peek` (not `cache_get`) to check absence without touching the very counters
        // under test.
        assert_eq!(
            crate::CachedPeek::cache_peek(&c, &2u32),
            None,
            "a failed factory inserts nothing"
        );
        assert_eq!(c.cache_size(), 1);
        assert_eq!(c.cache_hits(), Some(1), "step 2: still one hit");
        assert_eq!(c.cache_misses(), Some(1), "step 2: one miss, not two");
        assert_eq!(fired.load(Ordering::Relaxed), 0);
        assert_eq!(c.cache_evictions(), Some(0));

        // Step 3: the SAME absent key, factory Ok this time. One more miss (the key is still
        // absent going in), a fresh insert with nothing displaced, so no eviction.
        let v: Result<&mut u32, &'static str> =
            CachedGetOrSetAsync::async_cache_try_get_or_set_with_mut(&mut c, 2u32, || async {
                Ok(200u32)
            })
            .await;
        assert_eq!(v.ok(), Some(&mut 200u32));
        assert_eq!(c.cache_size(), 2);
        assert_eq!(c.cache_hits(), Some(1), "step 3: still one hit");
        assert_eq!(c.cache_misses(), Some(2), "step 3: two misses total");
        assert_eq!(fired.load(Ordering::Relaxed), 0);
        assert_eq!(c.cache_evictions(), Some(0));

        // Step 4: an EXPIRED key (key 3), factory Err. Counts a miss even though the key is
        // technically present-but-stale; the stale entry is left exactly alone (no eviction).
        c.set_with(3u32, 300u32).ttl(Duration::from_millis(1)).set();
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        let v: Result<&mut u32, &'static str> =
            CachedGetOrSetAsync::async_cache_try_get_or_set_with_mut(&mut c, 3u32, || async {
                Err("still nope")
            })
            .await;
        assert_eq!(v.err(), Some("still nope"));
        assert_eq!(c.cache_hits(), Some(1), "step 4: still one hit");
        assert_eq!(c.cache_misses(), Some(3), "step 4: three misses total");
        assert_eq!(
            fired.load(Ordering::Relaxed),
            0,
            "the stale entry is left alone on Err, not evicted"
        );
        assert_eq!(c.cache_evictions(), Some(0));

        // Step 5: the SAME expired key, factory Ok. One more miss, and NOW the stale entry is
        // displaced: exactly one eviction fires, counted once.
        let v: Result<&mut u32, &'static str> =
            CachedGetOrSetAsync::async_cache_try_get_or_set_with_mut(&mut c, 3u32, || async {
                Ok(301u32)
            })
            .await;
        assert_eq!(v.ok(), Some(&mut 301u32));
        assert_eq!(c.cache_hits(), Some(1), "step 5: still one hit");
        assert_eq!(c.cache_misses(), Some(4), "step 5: four misses total");
        assert_eq!(
            fired.load(Ordering::Relaxed),
            1,
            "the stale entry is evicted exactly once when finally displaced"
        );
        assert_eq!(c.cache_evictions(), Some(1));

        // Step 6: a final live hit on the just-replaced key 3, to confirm hits still advance
        // correctly after a run of misses.
        let v: Result<&mut u32, &'static str> =
            CachedGetOrSetAsync::async_cache_try_get_or_set_with_mut(&mut c, 3u32, || async {
                unreachable!("the factory must not run on a live hit")
            })
            .await;
        assert_eq!(v.ok(), Some(&mut 301u32));
        assert_eq!(c.cache_hits(), Some(2), "step 6: two hits total");
        assert_eq!(c.cache_misses(), Some(4), "step 6: no additional miss");
        assert_eq!(c.cache_evictions(), Some(1));
        assert_eq!(c.cache_size(), 3);
        assert_index_lockstep(&c, "after the mixed hit/miss/absent/expired sequence");
    }

    // ===========================================================================
    // Independent certification coverage (outside-in): the `split_off` pivot,
    // the two `evict_at` drain branches, deferred-callback panic safety, the
    // orphaned-stamp divergence, coarse-`Ord` keys, and the async liveness-check
    // behavior change.
    // ===========================================================================

    /// Insert a stamp into the expiry index WITHOUT a matching map entry, i.e. deliberately
    /// break the map/index lockstep invariant. Only reachable from inside this module; used
    /// to pin what the sweeps do if the invariant were ever violated.
    fn insert_orphan_stamp(
        cache: &mut TtlSortedCache<u32, u32>,
        key: u32,
        expiry: Option<crate::time::Instant>,
    ) {
        use super::{CacheArc, Stamped};
        cache.keys.insert(Stamped {
            expiry,
            key: Some(CacheArc::new(key)),
        });
    }

    /// The `key: None` sentinel must sort strictly BELOW every real key carrying the same
    /// expiry, and strictly below any never-expiring stamp. This is the single ordering fact
    /// the whole `split_off` boundary rests on.
    #[test]
    fn bound_sentinel_sorts_below_every_real_key_at_the_same_expiry() {
        use super::{CacheArc, Stamped};
        use crate::time::Instant;

        let t = Instant::now();
        let sentinel = Stamped::<u32>::bound(t);
        for k in [u32::MIN, 1u32, u32::MAX] {
            let real = Stamped {
                expiry: Some(t),
                key: Some(CacheArc::new(k)),
            };
            assert!(
                sentinel < real,
                "bound({t:?}) must sort below the real key {k} at the same expiry"
            );
        }
        // A never-expiring stamp sorts above every finite bound, no matter how far out.
        let never = Stamped {
            expiry: None,
            key: Some(CacheArc::new(0u32)),
        };
        assert!(sentinel < never, "None expiry sorts greatest");
        assert!(
            Stamped::<u32>::bound(t + Duration::from_secs(60 * 60 * 24 * 365)) < never,
            "no finite cutoff can ever reach a never-expiring stamp"
        );
        // And the sentinel is strictly ordered by expiry.
        assert!(sentinel < Stamped::<u32>::bound(t + Duration::from_nanos(1)));
    }

    /// Non-vacuity guard for the boundary tests: the population they use genuinely
    /// discriminates the pivot. Splitting the SAME index at `bound(cutoff - 1ns)`,
    /// `bound(cutoff)` and `bound(cutoff + 1ns)` must yield three different prefix sizes
    /// (0, 1, 2), so an off-by-one-tick pivot could not pass the shipped assertions.
    #[test]
    fn evict_at_pivot_choice_is_observable_at_the_tie() {
        use super::Stamped;

        for (shift_back, expected) in [(true, 0usize), (false, 1)] {
            let (cache, cutoff) = boundary_population();
            let pivot = if shift_back {
                Stamped::<u32>::bound(cutoff - Duration::from_nanos(1))
            } else {
                Stamped::<u32>::bound(cutoff)
            };
            let mut keys = cache.keys.clone();
            let prefix_len = keys.len() - keys.split_off(&pivot).len();
            assert_eq!(
                prefix_len, expected,
                "shift_back={shift_back}: the split prefix must be sensitive to the pivot"
            );
        }
        let (cache, cutoff) = boundary_population();
        let mut keys = cache.keys.clone();
        let past_tie = keys.len()
            - keys
                .split_off(&Stamped::<u32>::bound(cutoff + Duration::from_nanos(1)))
                .len();
        assert_eq!(
            past_tie, 2,
            "a pivot one tick past the tie would sweep the tied entry too"
        );
        // The shipped pivot is the middle one, so both off-by-one-tick mutants are caught.
        let (mut cache, cutoff) = boundary_population();
        assert_eq!(cache.evict_at(cutoff), 1);
    }

    /// `split_off` on an empty index must be a clean no-op, not a panic or a leaked
    /// half-swapped tree.
    #[test]
    fn evict_at_on_an_empty_index_is_a_noop() {
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        let now = crate::time::Instant::now();
        assert_eq!(cache.evict_at(now), 0);
        assert_eq!(cache.evict_at(now + Duration::from_secs(1000)), 0);
        assert!(cache.keys.is_empty());
        assert!(cache.map.is_empty());
        assert_eq!(cache.cache_evictions(), Some(0));
        assert_index_lockstep(&cache, "empty evict_at");

        // Still usable afterwards.
        cache.set(1u32, 1u32);
        assert_index_lockstep(&cache, "after reuse");
        assert_eq!(cache.cache_get(&1u32), Some(&1u32));
    }

    /// An index that is ENTIRELY expired: `split_off` returns an empty live half, so the
    /// whole tree is drained and `self.keys` must end up empty (not the detached prefix).
    #[test]
    fn evict_at_with_an_entirely_expired_index_drains_everything() {
        for with_callback in [false, true] {
            let fired = Arc::new(AtomicUsize::new(0));
            let fired2 = fired.clone();
            let mut builder = TtlSortedCache::<u32, u32>::builder().ttl(Duration::from_secs(60));
            if with_callback {
                builder = builder.on_evict(move |_k: &u32, _v: &u32| {
                    fired2.fetch_add(1, Ordering::Relaxed);
                });
            }
            let mut cache = builder.build().unwrap();
            let cutoff = crate::time::Instant::now() + Duration::from_secs(1);
            for k in 0u32..8 {
                insert_raw(
                    &mut cache,
                    k,
                    k,
                    Some(cutoff - Duration::from_micros(u64::from(k) + 1)),
                );
            }
            assert_eq!(cache.evict_at(cutoff), 8, "with_callback={with_callback}");
            assert!(cache.keys.is_empty(), "the index must be fully drained");
            assert!(cache.map.is_empty());
            assert_eq!(cache.cache_evictions(), Some(8));
            assert_eq!(
                fired.load(Ordering::Relaxed),
                usize::from(with_callback) * 8
            );
            assert_index_lockstep(&cache, "after draining every entry");
            assert_eq!(cache.evict_at(cutoff), 0, "a repeat sweep drops nothing");
        }
    }

    /// An index that is ENTIRELY live: the detached prefix must be empty and the live half
    /// must be swapped back intact, in order.
    #[test]
    fn evict_at_with_an_entirely_live_index_drops_nothing() {
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        let cutoff = crate::time::Instant::now() + Duration::from_secs(1);
        for k in 0u32..8 {
            let expiry = if k % 2 == 0 {
                Some(cutoff + Duration::from_micros(u64::from(k) + 1))
            } else {
                None
            };
            insert_raw(&mut cache, k, k * 10, expiry);
        }
        let before: Vec<Option<u32>> = cache
            .keys
            .iter()
            .map(|s| s.key.as_ref().map(|k| *k.0))
            .collect();

        assert_eq!(cache.evict_at(cutoff), 0);
        assert_eq!(cache.map.len(), 8);
        assert_eq!(cache.cache_evictions(), Some(0));
        let after: Vec<Option<u32>> = cache
            .keys
            .iter()
            .map(|s| s.key.as_ref().map(|k| *k.0))
            .collect();
        assert_eq!(before, after, "the live half is swapped back in order");
        assert_index_lockstep(&cache, "after an all-live evict_at");
    }

    /// The degenerate single-element cases at the tie: one entry exactly at the cutoff is
    /// kept, one entry a tick earlier is swept, and one never-expiring entry is kept.
    #[test]
    fn evict_at_single_element_exactly_at_the_tie_is_kept() {
        let cutoff = crate::time::Instant::now() + Duration::from_secs(1);
        let cases: [(Option<Duration>, usize); 3] = [
            (Some(Duration::ZERO), 0),          // expiry == cutoff -> live
            (Some(Duration::from_nanos(1)), 1), // expiry == cutoff - 1ns -> swept
            (None, 0),                          // never expires -> live
        ];
        for (back_off, expected) in cases {
            let mut cache = TtlSortedCache::<u32, u32>::builder()
                .ttl(Duration::from_secs(60))
                .build()
                .unwrap();
            let expiry = back_off.map(|d| cutoff - d);
            insert_raw(&mut cache, 1u32, 10u32, expiry);
            assert_eq!(
                old_range_expired_count(&cache, cutoff),
                expected,
                "reference: the old range agrees for back_off={back_off:?}"
            );
            assert_eq!(
                cache.evict_at(cutoff),
                expected,
                "single-element boundary for back_off={back_off:?}"
            );
            assert_eq!(cache.map.len(), 1 - expected);
            assert_index_lockstep(&cache, "single-element boundary");
        }
    }

    /// DIVERGENCE FROM THE OLD CODE (intentional): the old sweep range started at
    /// `Included(bound(min_instant))`, so an entry expiring BEFORE the cache was built would
    /// never have been swept. `split_off` has no lower bound, so such an entry is now swept.
    /// It is unreachable through the public API (every stored expiry is `insert_time + ttl`
    /// and every insert happens after `min_instant`), so this is a strict improvement, but it
    /// is a real behavioral difference and is pinned here.
    #[test]
    fn evict_at_sweeps_entries_older_than_min_instant_unlike_the_old_lower_bound() {
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        let min_instant = cache.min_instant;
        let cutoff = min_instant + Duration::from_secs(1);
        // Below `min_instant`: outside the old `Included(bound(min_instant))..` range.
        insert_raw(
            &mut cache,
            1u32,
            10u32,
            Some(min_instant - Duration::from_nanos(1)),
        );
        insert_raw(&mut cache, 2u32, 20u32, Some(min_instant));

        assert_eq!(
            old_range_expired_count(&cache, cutoff),
            1,
            "the old lower bound excluded the pre-min_instant entry"
        );
        assert_eq!(
            cache.evict_at(cutoff),
            2,
            "the front-driven sweep has no lower bound and reaps both"
        );
        assert!(cache.map.is_empty());
        assert_index_lockstep(&cache, "after sweeping below min_instant");
    }

    /// Deferred callbacks must still fire in expiry order, exactly once per entry actually
    /// removed from the map, with the return value and the eviction counter agreeing.
    #[test]
    fn evict_at_fires_on_evict_in_expiry_order_after_every_map_removal() {
        let log = Arc::new(std::sync::Mutex::new(Vec::<(u32, u32)>::new()));
        let log2 = log.clone();
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .on_evict(move |k: &u32, v: &u32| log2.lock().unwrap().push((*k, *v)))
            .build()
            .unwrap();
        let cutoff = crate::time::Instant::now() + Duration::from_secs(1);
        // Inserted in a scrambled order; the index sorts them by expiry regardless.
        for k in [3u32, 0, 4, 1, 2] {
            insert_raw(
                &mut cache,
                k,
                k * 10,
                Some(cutoff - Duration::from_micros(10 - u64::from(k))),
            );
        }
        insert_raw(&mut cache, 100u32, 1000u32, Some(cutoff));

        let dropped = cache.evict_at(cutoff);
        assert_eq!(dropped, 5, "the returned count is the number of drops");
        assert_eq!(
            cache.cache_evictions(),
            Some(5),
            "the counter agrees with the return value"
        );
        assert_eq!(
            *log.lock().unwrap(),
            vec![(0, 0), (1, 10), (2, 20), (3, 30), (4, 40)],
            "callbacks fire in ascending expiry order, once per removed entry"
        );
        assert_eq!(cache.map.len(), 1);
        assert_index_lockstep(&cache, "after an ordered deferred sweep");
    }

    /// A callback that panics on a MIDDLE entry: every map removal has already happened, so
    /// the index and the map stay in lockstep and the counter reflects all of them.
    #[test]
    fn evict_at_on_evict_panic_on_a_middle_callback_leaves_the_index_in_lockstep() {
        for panic_on in [0usize, 2, 5] {
            let seen = Arc::new(AtomicUsize::new(0));
            let seen2 = seen.clone();
            let mut cache = TtlSortedCache::<u32, u32>::builder()
                .ttl(Duration::from_secs(60))
                .on_evict(move |_k: &u32, _v: &u32| {
                    if seen2.fetch_add(1, Ordering::Relaxed) == panic_on {
                        panic!("boom");
                    }
                })
                .build()
                .unwrap();
            let cutoff = crate::time::Instant::now() + Duration::from_secs(1);
            for k in 0u32..6 {
                insert_raw(
                    &mut cache,
                    k,
                    k,
                    Some(cutoff - Duration::from_micros(u64::from(k) + 1)),
                );
            }
            insert_raw(&mut cache, 100u32, 100u32, Some(cutoff));

            let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = cache.evict_at(cutoff);
            }));
            assert!(caught.is_err(), "panic_on={panic_on}: must propagate");
            assert_index_lockstep(&cache, "after a mid-drain callback panic");
            assert_eq!(
                cache.map.len(),
                1,
                "panic_on={panic_on}: every expired entry left the map before any callback"
            );
            assert!(cache.map.contains_key(&100u32));
            assert_eq!(
                cache.cache_evictions(),
                Some(6),
                "panic_on={panic_on}: the counter is bumped before the callbacks"
            );
            assert_eq!(
                seen.load(Ordering::Relaxed),
                panic_on + 1,
                "panic_on={panic_on}: unwinding stops the remaining callbacks"
            );
            // The survivor is still reachable through the index.
            assert_eq!(cache.retain_latest(0, false), 1);
            assert_index_lockstep(&cache, "after trimming the survivor");
        }
    }

    /// The no-callback branch and the callback branch of `evict_at` must produce identical
    /// return values, identical eviction counts and identical resulting state for the same
    /// input. Only the callback branch is directly asserted elsewhere.
    #[test]
    fn evict_at_drain_branches_agree_on_count_state_and_evictions() {
        fn populate(cache: &mut TtlSortedCache<u32, u32>, cutoff: crate::time::Instant) {
            for k in 0u32..24 {
                let expiry = match k % 4 {
                    // Expired, with colliding instants in pairs.
                    0 | 1 => Some(cutoff - Duration::from_micros(u64::from(k / 2) + 1)),
                    // Live, half of them exactly on the tie.
                    2 => Some(cutoff + Duration::from_micros(u64::from(k % 3))),
                    _ => None,
                };
                insert_raw(cache, k, k * 7, expiry);
            }
        }

        let cutoff = crate::time::Instant::now() + Duration::from_secs(1);

        let mut plain = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        populate(&mut plain, cutoff);

        let fired = Arc::new(AtomicUsize::new(0));
        let fired2 = fired.clone();
        let mut with_cb = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .on_evict(move |_k: &u32, _v: &u32| {
                fired2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();
        populate(&mut with_cb, cutoff);

        let plain_dropped = plain.evict_at(cutoff);
        let cb_dropped = with_cb.evict_at(cutoff);

        assert_eq!(
            plain_dropped, cb_dropped,
            "both drain branches drop the same count"
        );
        assert_eq!(plain_dropped, 12, "half the population is expired");
        assert_eq!(plain.cache_evictions(), with_cb.cache_evictions());
        assert_eq!(plain.cache_evictions(), Some(12));
        assert_eq!(fired.load(Ordering::Relaxed), 12);

        let plain_keys: Vec<u32> = {
            let mut v: Vec<u32> = plain.map.keys().copied().collect();
            v.sort_unstable();
            v
        };
        let cb_keys: Vec<u32> = {
            let mut v: Vec<u32> = with_cb.map.keys().copied().collect();
            v.sort_unstable();
            v
        };
        assert_eq!(plain_keys, cb_keys, "identical surviving map contents");

        let plain_index: Vec<(Option<crate::time::Instant>, Option<u32>)> = plain
            .keys
            .iter()
            .map(|s| (s.expiry, s.key.as_ref().map(|k| *k.0)))
            .collect();
        let cb_index: Vec<(Option<crate::time::Instant>, Option<u32>)> = with_cb
            .keys
            .iter()
            .map(|s| (s.expiry, s.key.as_ref().map(|k| *k.0)))
            .collect();
        assert_eq!(plain_index, cb_index, "identical surviving index contents");
        assert_index_lockstep(&plain, "no-callback branch");
        assert_index_lockstep(&with_cb, "callback branch");
    }

    /// PINS THE ORPHAN DIVERGENCE. An index stamp with no map entry is unreachable through
    /// the public API (see the module certification notes: every insert stamps exactly one
    /// entry and every removal drops the rebuilt stamp), but if it ever occurred, `evict_at`
    /// would COUNT it as a drop in its return value while NOT counting it as an eviction.
    /// This test documents that asymmetry so a future change to either counter is deliberate.
    #[test]
    fn evict_at_orphaned_stamp_is_counted_as_a_drop_but_not_as_an_eviction() {
        for with_callback in [false, true] {
            let fired = Arc::new(AtomicUsize::new(0));
            let fired2 = fired.clone();
            let mut builder = TtlSortedCache::<u32, u32>::builder().ttl(Duration::from_secs(60));
            if with_callback {
                builder = builder.on_evict(move |_k: &u32, _v: &u32| {
                    fired2.fetch_add(1, Ordering::Relaxed);
                });
            }
            let mut cache = builder.build().unwrap();
            let cutoff = crate::time::Instant::now() + Duration::from_secs(1);
            insert_raw(
                &mut cache,
                1u32,
                10u32,
                Some(cutoff - Duration::from_micros(2)),
            );
            // No map entry for key 9.
            insert_orphan_stamp(&mut cache, 9u32, Some(cutoff - Duration::from_micros(1)));
            assert_eq!(cache.keys.len(), 2);
            assert_eq!(cache.map.len(), 1);

            assert_eq!(
                cache.evict_at(cutoff),
                2,
                "with_callback={with_callback}: the return value counts index drops"
            );
            assert_eq!(
                cache.cache_evictions(),
                Some(1),
                "with_callback={with_callback}: only real map removals are evictions"
            );
            assert_eq!(fired.load(Ordering::Relaxed), usize::from(with_callback));
            // The orphan is gone, so the cache is back in lockstep afterwards.
            assert_index_lockstep(&cache, "after sweeping an orphaned stamp");
        }
    }

    /// The same divergence through `retain_latest_at`: `retain_drop_count` is derived from
    /// `map.len()` while the pop loop walks `keys`, so an orphan makes the trim consume one
    /// index slot without freeing a map entry, leaving the cache above the requested count.
    #[test]
    fn retain_latest_at_orphaned_stamp_diverges_from_the_map_length() {
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        let base = crate::time::Instant::now() + Duration::from_secs(60);
        // The orphan sorts FIRST, so the trim pops it before any real entry.
        insert_orphan_stamp(&mut cache, 9u32, Some(base));
        for k in 0u32..3 {
            insert_raw(
                &mut cache,
                k,
                k,
                Some(base + Duration::from_micros(u64::from(k) + 1)),
            );
        }
        assert_eq!(cache.map.len(), 3);
        assert_eq!(cache.keys.len(), 4);

        // Ask to keep 2 of 3 map entries: one drop. The orphan absorbs it.
        assert_eq!(cache.retain_latest_at(2, None), 1);
        assert_eq!(
            cache.map.len(),
            3,
            "the orphan absorbed the trim, so no map entry was freed"
        );
        assert_eq!(cache.cache_evictions(), Some(0));
        assert_eq!(cache.keys.len(), 3, "the cache is back in lockstep");
        assert_index_lockstep(&cache, "after the orphan absorbed a trim");
        // A second trim now behaves normally.
        assert_eq!(cache.retain_latest_at(2, None), 1);
        assert_eq!(cache.map.len(), 2);
        assert_eq!(cache.cache_evictions(), Some(1));
    }

    /// `retain_latest_at` pops one stamp at a time and removes its map entry, counts the
    /// eviction, and only then fires the callback -- matching `evict_at` / `retain`. So a
    /// panicking callback still leaves the two structures in lockstep AND the entry whose
    /// callback panicked is still counted, since the counter is bumped BEFORE the callback runs.
    #[test]
    fn retain_latest_at_on_evict_panic_leaves_lockstep_and_counts_the_eviction() {
        let seen = Arc::new(AtomicUsize::new(0));
        let seen2 = seen.clone();
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .on_evict(move |_k: &u32, _v: &u32| {
                if seen2.fetch_add(1, Ordering::Relaxed) == 1 {
                    panic!("boom");
                }
            })
            .build()
            .unwrap();
        for k in 0u32..5 {
            cache
                .set_with(k, k * 10)
                .ttl(Duration::from_secs(10 * (u64::from(k) + 1)))
                .set();
        }

        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = cache.retain_latest(1, false);
        }));
        assert!(caught.is_err(), "the callback panic must propagate");

        assert_index_lockstep(&cache, "after a retain_latest callback panic");
        assert_eq!(
            cache.map.len(),
            3,
            "two entries were removed before the panic"
        );
        assert_eq!(seen.load(Ordering::Relaxed), 2);
        assert_eq!(
            cache.cache_evictions(),
            Some(2),
            "both removals are counted: the counter is bumped before the callback runs"
        );
        // The cache remains usable and fully reachable through the index.
        assert_eq!(cache.retain_latest(0, false), 3);
        assert!(cache.map.is_empty());
        assert!(cache.keys.is_empty());
    }

    /// The tie boundary reached through the SECOND phase of `retain_latest_at` (after the
    /// size trim is satisfied), which uses a separate `expiry < cutoff` comparison rather
    /// than the `split_off` pivot. An entry expiring exactly at the cutoff must stop the
    /// sweep, matching `evict_at`.
    #[test]
    fn retain_latest_at_tie_stops_the_sweep_after_the_size_trim() {
        // 4 entries, keep 3 -> retain_drop_count = 1: entry 1 (strictly before the cutoff) is
        // dropped by the trim, then entry 2 sits exactly ON the cutoff and must stop the sweep.
        let (mut cache, cutoff) = boundary_population();
        assert_eq!(
            cache.retain_latest_at(3, Some(cutoff)),
            1,
            "the tied entry must not be swept by the post-trim expiry loop"
        );
        assert!(!cache.map.contains_key(&1u32));
        assert!(cache.map.contains_key(&2u32), "the exact tie survives");
        assert!(cache.map.contains_key(&3u32));
        assert!(cache.map.contains_key(&4u32));
        assert_index_lockstep(&cache, "after a post-trim tie stop");

        // One tick later the tie IS expired and the sweep continues past the trim.
        let (mut cache, cutoff) = boundary_population();
        assert_eq!(
            cache.retain_latest_at(3, Some(cutoff + Duration::from_nanos(1))),
            2
        );
        assert!(!cache.map.contains_key(&2u32));
        assert!(cache.map.contains_key(&3u32));
        assert_index_lockstep(&cache, "after sweeping past the tie");
    }

    // --- Coarse `Eq`/`Ord` keys: two distinct values that compare equal ------------------

    /// A key whose `Eq`/`Ord`/`Hash` consider only `label`, so two values carrying different
    /// `payload`s compare EQUAL. `set_inner`'s occupied branch reuses the stored `Arc`, and
    /// `BTreeSet::insert` keeps the incumbent on an equal insert, so the index and the map
    /// can hold different-but-equal `Stamped`s.
    #[derive(Clone, Debug)]
    struct CoarseKey {
        label: &'static str,
        payload: u32,
    }

    impl Hash for CoarseKey {
        fn hash<H: Hasher>(&self, state: &mut H) {
            self.label.hash(state);
        }
    }
    impl PartialEq for CoarseKey {
        fn eq(&self, other: &Self) -> bool {
            self.label == other.label
        }
    }
    impl Eq for CoarseKey {}
    impl PartialOrd for CoarseKey {
        fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
            Some(self.cmp(other))
        }
    }
    impl Ord for CoarseKey {
        fn cmp(&self, other: &Self) -> CmpOrdering {
            self.label.cmp(other.label)
        }
    }

    /// Overwriting with an equal-but-distinct key must keep exactly one map entry and one
    /// stamp, and the STORED key is the FIRST-inserted payload (the occupied branch reuses
    /// the existing `Arc`, matching `HashMap`'s own "insert keeps the incumbent key" rule).
    /// `on_evict` and `cache_remove_entry` therefore report the first payload, not the last.
    #[test]
    fn coarse_key_overwrite_keeps_lockstep_and_reports_the_first_stored_key() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::<u32>::new()));
        let seen2 = seen.clone();
        let mut cache = TtlSortedCache::<CoarseKey, u32>::builder()
            .ttl(Duration::from_secs(60))
            .on_evict(move |k: &CoarseKey, _v: &u32| seen2.lock().unwrap().push(k.payload))
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
        assert_eq!(first.cmp(&second), CmpOrdering::Equal);

        cache.cache_set(first.clone(), 10u32);
        assert_eq!(cache.cache_set(second.clone(), 20u32), Some(10u32));
        assert_eq!(cache.map.len(), 1);
        assert_eq!(cache.keys.len(), 1, "no duplicate stamp for an equal key");
        assert_index_lockstep(&cache, "after a coarse-key overwrite");

        // The stored key is the first payload, and the index stamp shares its `Arc`.
        let entry = cache.map.values().next().expect("one entry");
        assert_eq!(
            entry.key.0.payload, 1,
            "the occupied branch reuses the originally stored key"
        );
        let stamp = cache.keys.iter().next().expect("one stamp");
        assert!(
            Arc::ptr_eq(
                &stamp.key.as_ref().expect("real key").0,
                &cache.map.values().next().expect("one entry").key.0
            ),
            "the index stamp and the map entry share the same key Arc"
        );

        // Removal by the SECOND (equal) key still finds the entry, and hands the callback and
        // the `cache_remove_entry` return the FIRST payload.
        let removed = cache.cache_remove_entry(&second).expect("present");
        assert_eq!(removed.0.payload, 1, "the stored key is returned");
        assert_eq!(removed.1, 20u32);
        assert_eq!(*seen.lock().unwrap(), vec![1u32]);
        assert!(cache.map.is_empty());
        assert!(cache.keys.is_empty());
        assert_index_lockstep(&cache, "after a coarse-key removal");
    }

    /// The same coarse key across an expiry change: the stale stamp must be removed by the
    /// rebuilt `old.as_stamped()`, which carries the shared `Arc`, so no duplicate survives
    /// and the sweep order follows the NEW expiry.
    #[test]
    fn coarse_key_overwrite_with_a_new_expiry_replaces_exactly_one_stamp() {
        let mut cache = TtlSortedCache::<CoarseKey, u32>::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        let a1 = CoarseKey {
            label: "a",
            payload: 1,
        };
        let a2 = CoarseKey {
            label: "a",
            payload: 2,
        };
        let b = CoarseKey {
            label: "b",
            payload: 9,
        };
        cache.set_with(a1, 10u32).ttl(Duration::ZERO).set();
        cache.set_with(b, 90u32).ttl(Duration::from_secs(30)).set();
        assert_index_lockstep(&cache, "coarse pair");

        // UNCHANGED expiry (None -> None) with a different-but-equal key: the new stamp is
        // Ord-equal to the incumbent, so `BTreeSet::insert` is a no-op and the stale-stamp
        // removal MUST be skipped -- removing it here would delete the only stamp for a live
        // map entry and orphan it.
        assert_eq!(
            cache.set_with(a2.clone(), 10u32).ttl(Duration::ZERO).set(),
            Some(10u32)
        );
        assert_eq!(cache.map.len(), 2);
        assert_eq!(
            cache.keys.len(),
            2,
            "the single stamp survives the overwrite"
        );
        assert_index_lockstep(&cache, "after a same-expiry coarse-key overwrite");

        // None -> Some: the never-expires stamp must go.
        assert_eq!(
            cache
                .set_with(a2.clone(), 11u32)
                .ttl(Duration::from_secs(5))
                .set(),
            Some(10u32)
        );
        assert_eq!(cache.cache_evictions(), Some(0), "no expired displacement");
        assert_eq!(cache.map.len(), 2);
        assert_eq!(cache.keys.len(), 2, "the stale stamp is gone");
        assert_index_lockstep(&cache, "after a coarse-key re-stamp");

        // "a" now expires first, so it is trimmed first.
        assert_eq!(cache.retain_latest(1, false), 1);
        assert!(!cache.map.contains_key(&a2));
        assert_index_lockstep(&cache, "after trimming the re-stamped coarse key");
    }

    /// A long mixed sequence over the public surface: the lockstep invariant must hold after
    /// every single operation. This is the empirical half of the "an orphaned stamp is
    /// unreachable by construction" claim.
    #[test]
    fn lockstep_holds_across_a_mixed_public_operation_sequence() {
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_millis(30))
            .max_size(5)
            .on_evict(|_k: &u32, _v: &u32| {})
            .build()
            .unwrap();
        for round in 0u32..40 {
            let k = round % 7;
            match round % 8 {
                0 => {
                    cache.set(k, round);
                }
                1 => {
                    cache.set_with(k, round).ttl(Duration::ZERO).set();
                }
                2 => {
                    cache
                        .set_with(k, round)
                        .ttl(Duration::from_millis(10))
                        .evict()
                        .set();
                }
                3 => {
                    let _ = cache.cache_get(&k);
                }
                4 => {
                    let _ = cache.cache_remove(&k);
                }
                5 => {
                    let _ = cache.cache_get_or_set_with_mut(k, || round);
                }
                6 => {
                    cache.retain(|kk, _v| *kk != round % 5);
                }
                _ => {
                    let _ = cache.retain_latest(3, round % 16 == 7);
                }
            }
            assert_index_lockstep(&cache, "mixed sequence");
            assert!(cache.map.len() <= 5 || round % 8 == 5);
        }
        let _ = cache.evict();
        assert_index_lockstep(&cache, "after the final sweep");
        cache.cache_clear();
        assert_index_lockstep(&cache, "after clear");
    }

    /// `set_inner` samples the clock ONCE and judges the displaced entry against that sample.
    /// The classification itself is pinned here with exact expiries; the sub-microsecond
    /// window between the sample and the map write is NOT observable from a test (see the
    /// certification notes) because the store exposes no clock injection point.
    #[test]
    fn set_inner_classifies_the_displaced_entry_against_its_clock_sample() {
        let fired = Arc::new(AtomicUsize::new(0));
        let fired2 = fired.clone();
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .on_evict(move |_k: &u32, _v: &u32| {
                fired2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();

        // Displaced entry already past its deadline -> filtered from the return, evicted.
        insert_raw(
            &mut cache,
            1u32,
            10u32,
            Some(crate::time::Instant::now() - Duration::from_nanos(1)),
        );
        assert_eq!(cache.cache_set(1u32, 11u32), None);
        assert_eq!(fired.load(Ordering::Relaxed), 1);
        assert_eq!(cache.cache_evictions(), Some(1));
        assert_index_lockstep(&cache, "after displacing an expired entry");

        // Displaced entry comfortably in the future -> returned as live, no eviction.
        insert_raw(
            &mut cache,
            2u32,
            20u32,
            Some(crate::time::Instant::now() + Duration::from_secs(3600)),
        );
        assert_eq!(cache.cache_set(2u32, 21u32), Some(20u32));
        assert_eq!(fired.load(Ordering::Relaxed), 1);
        assert_eq!(cache.cache_evictions(), Some(1));
        assert_index_lockstep(&cache, "after displacing a live entry");
    }

    // --- Item 7: the async liveness-check behavior change -------------------------------

    /// (c) A PANICKING factory future: the expired entry is left in place, `on_evict` never
    /// fires and nothing is counted. Under the previous `cache_get`-based shape the entry was
    /// already swept (and `on_evict` already fired) before the future was ever polled.
    #[cfg(feature = "async")]
    #[tokio::test]
    async fn async_cache_get_or_set_with_mut_factory_panic_keeps_the_expired_entry() {
        use crate::CachedGetOrSetAsync;

        let fired = Arc::new(AtomicUsize::new(0));
        let fired2 = fired.clone();
        let mut c: TtlSortedCache<u32, u32> = TtlSortedCache::builder()
            .ttl(Duration::from_millis(20))
            .on_evict(move |_k: &u32, _v: &u32| {
                fired2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();
        c.cache_set(1u32, 100u32);
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        c.cache_reset_metrics();

        {
            let mut fut = Box::pin(CachedGetOrSetAsync::async_cache_get_or_set_with_mut(
                &mut c,
                1u32,
                || async { panic!("factory boom") },
            ));
            let waker = std::task::Waker::noop();
            let mut cx = std::task::Context::from_waker(waker);
            let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = std::future::Future::poll(fut.as_mut(), &mut cx);
            }));
            assert!(caught.is_err(), "the factory panic must propagate");
        }

        assert_eq!(
            fired.load(Ordering::Relaxed),
            0,
            "on_evict must not fire when the factory panics"
        );
        assert_eq!(c.cache_evictions(), Some(0));
        assert_eq!(c.cache_size(), 1, "the expired entry is still stored");
        assert_eq!(
            c.cache_misses(),
            Some(1),
            "the miss is counted before the factory runs"
        );
        // The stale value is still observable through the expiry-status peek.
        assert_eq!(
            crate::CloneCached::cache_peek_with_expiry_status(&c, &1u32),
            (Some(100u32), true),
            "the stale value survives the panic and is reported as expired"
        );
        assert_index_lockstep(&c, "after an async factory panic");
    }

    /// (a) NORMAL COMPLETION: the eviction of the displaced expired entry now happens AFTER
    /// the factory resolves, not before it is polled. Observed through the callback ordering.
    #[cfg(feature = "async")]
    #[tokio::test]
    async fn async_cache_get_or_set_with_mut_fires_on_evict_after_the_factory_completes() {
        use crate::CachedGetOrSetAsync;

        let log = Arc::new(std::sync::Mutex::new(Vec::<&'static str>::new()));
        let log2 = log.clone();
        let mut c: TtlSortedCache<u32, u32> = TtlSortedCache::builder()
            .ttl(Duration::from_millis(20))
            .on_evict(move |_k: &u32, _v: &u32| log2.lock().unwrap().push("evict"))
            .build()
            .unwrap();
        c.cache_set(1u32, 100u32);
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;

        let log3 = log.clone();
        let v = CachedGetOrSetAsync::async_cache_get_or_set_with_mut(&mut c, 1u32, || async move {
            log3.lock().unwrap().push("factory");
            200u32
        })
        .await;
        assert_eq!(*v, 200u32);
        assert_eq!(
            *log.lock().unwrap(),
            vec!["factory", "evict"],
            "the displaced expired entry is evicted after the factory resolves"
        );
        assert_eq!(c.cache_evictions(), Some(1));
        assert_eq!(c.cache_size(), 1);
        assert_index_lockstep(&c, "after an async replacement");
    }

    /// The fallible async sibling now uses the same in-place liveness check as the other
    /// three `cache_*get_or_set_with_mut` variants instead of routing through `cache_get`
    /// (which would sweep the expired entry, firing `on_evict`, before the factory is ever
    /// polled). On `Err` the expired entry is left exactly in place, matching the sync
    /// fallible sibling.
    #[cfg(feature = "async")]
    #[tokio::test]
    async fn async_try_get_or_set_with_mut_leaves_the_expired_entry_alone_on_err() {
        use crate::CachedGetOrSetAsync;

        let fired = Arc::new(AtomicUsize::new(0));
        let fired2 = fired.clone();
        let mut c: TtlSortedCache<u32, u32> = TtlSortedCache::builder()
            .ttl(Duration::from_millis(20))
            .on_evict(move |_k: &u32, _v: &u32| {
                fired2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();
        c.cache_set(1u32, 100u32);
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        c.cache_reset_metrics();

        let out: Result<&mut u32, &'static str> =
            CachedGetOrSetAsync::async_cache_try_get_or_set_with_mut(&mut c, 1u32, || async {
                Err("nope")
            })
            .await;
        assert_eq!(out.err(), Some("nope"));
        assert_eq!(
            c.cache_size(),
            1,
            "the fallible async path leaves the expired entry alone on Err"
        );
        assert_eq!(fired.load(Ordering::Relaxed), 0, "on_evict must not fire");
        assert_eq!(c.cache_evictions(), Some(0));
        assert_eq!(
            c.cache_misses(),
            Some(1),
            "the miss is counted before the factory runs"
        );
        assert_index_lockstep(&c, "after a failed async try factory");

        // The SYNC fallible sibling agrees: the expired entry is left in place.
        let mut c2: TtlSortedCache<u32, u32> = TtlSortedCache::builder()
            .ttl(Duration::from_millis(20))
            .build()
            .unwrap();
        c2.cache_set(1u32, 100u32);
        std::thread::sleep(std::time::Duration::from_millis(60));
        let out: Result<&mut u32, &'static str> =
            c2.cache_try_get_or_set_with_mut(1u32, || Err("nope"));
        assert_eq!(out.err(), Some("nope"));
        assert_eq!(
            c2.cache_size(),
            1,
            "the sync fallible path leaves the expired entry alone"
        );
        assert_eq!(c2.cache_evictions(), Some(0));
    }

    /// (b) CANCELLATION, contrasted directly between the two async methods on an identical
    /// starting state. Both now agree: the expired entry survives an in-flight future being
    /// dropped, and `on_evict` never fires for it.
    #[cfg(feature = "async")]
    #[tokio::test]
    async fn async_get_or_set_variants_agree_on_cancellation_over_an_expired_entry() {
        use crate::CachedGetOrSetAsync;

        async fn expired_cache() -> (TtlSortedCache<u32, u32>, Arc<AtomicUsize>) {
            let fired = Arc::new(AtomicUsize::new(0));
            let fired2 = fired.clone();
            let mut c: TtlSortedCache<u32, u32> = TtlSortedCache::builder()
                .ttl(Duration::from_millis(20))
                .on_evict(move |_k: &u32, _v: &u32| {
                    fired2.fetch_add(1, Ordering::Relaxed);
                })
                .build()
                .unwrap();
            c.cache_set(1u32, 100u32);
            tokio::time::sleep(std::time::Duration::from_millis(60)).await;
            (c, fired)
        }

        // Infallible: nothing observable happens to the entry.
        let (mut c, fired) = expired_cache().await;
        {
            let mut fut = Box::pin(CachedGetOrSetAsync::async_cache_get_or_set_with_mut(
                &mut c,
                1u32,
                std::future::pending::<u32>,
            ));
            let waker = std::task::Waker::noop();
            let mut cx = std::task::Context::from_waker(waker);
            assert!(matches!(
                std::future::Future::poll(fut.as_mut(), &mut cx),
                std::task::Poll::Pending
            ));
        }
        assert_eq!(c.cache_size(), 1);
        assert_eq!(fired.load(Ordering::Relaxed), 0);
        assert_eq!(c.cache_evictions(), Some(0));
        assert_index_lockstep(&c, "after a cancelled infallible async factory");

        // Fallible: now agrees -- the expired entry is likewise left alone by cancellation.
        let (mut c, fired) = expired_cache().await;
        {
            let mut fut = Box::pin(CachedGetOrSetAsync::async_cache_try_get_or_set_with_mut::<
                _,
                _,
                &'static str,
            >(&mut c, 1u32, || {
                std::future::pending::<Result<u32, &'static str>>()
            }));
            let waker = std::task::Waker::noop();
            let mut cx = std::task::Context::from_waker(waker);
            assert!(matches!(
                std::future::Future::poll(fut.as_mut(), &mut cx),
                std::task::Poll::Pending
            ));
        }
        assert_eq!(
            c.cache_size(),
            1,
            "the fallible async path also leaves the expired entry alone when cancelled"
        );
        assert_eq!(fired.load(Ordering::Relaxed), 0);
        assert_eq!(c.cache_evictions(), Some(0));
        assert_index_lockstep(&c, "after a cancelled fallible async factory");
    }

    /// (d) `Ok` completion for the fallible async variant: net end state/counters match the
    /// pre-fix behavior, but `on_evict` now fires AFTER the factory resolves (observed via
    /// callback ordering) and the insert takes the OCCUPIED `set_inner` branch (reusing the
    /// stored key `Arc`) rather than a vacant one, because the expired entry was never swept.
    #[cfg(feature = "async")]
    #[tokio::test]
    async fn async_try_get_or_set_with_mut_fires_on_evict_after_the_factory_completes() {
        use crate::CachedGetOrSetAsync;

        let log = Arc::new(std::sync::Mutex::new(Vec::<&'static str>::new()));
        let log2 = log.clone();
        let mut c: TtlSortedCache<u32, u32> = TtlSortedCache::builder()
            .ttl(Duration::from_millis(20))
            .on_evict(move |_k: &u32, _v: &u32| log2.lock().unwrap().push("evict"))
            .build()
            .unwrap();
        c.cache_set(1u32, 100u32);
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;

        let log3 = log.clone();
        let out: Result<&mut u32, &'static str> =
            CachedGetOrSetAsync::async_cache_try_get_or_set_with_mut(&mut c, 1u32, || async move {
                log3.lock().unwrap().push("factory");
                Ok(200u32)
            })
            .await;
        assert_eq!(out.ok(), Some(&mut 200u32));
        assert_eq!(
            *log.lock().unwrap(),
            vec!["factory", "evict"],
            "the displaced expired entry is evicted after the factory resolves"
        );
        assert_eq!(c.cache_evictions(), Some(1));
        assert_eq!(c.cache_size(), 1);
        assert_index_lockstep(&c, "after an async fallible replacement");
    }

    /// Cross-variant agreement table: sync infallible, sync fallible, async infallible, and
    /// async fallible must all behave identically on an expired-entry key for each terminal
    /// outcome the family shares, so a future change to one cannot silently desync the group.
    #[cfg(feature = "async")]
    #[tokio::test]
    async fn get_or_set_family_agrees_on_expired_entry_across_variants() {
        use crate::CachedGetOrSetAsync;

        fn expired_cache() -> TtlSortedCache<u32, u32> {
            let mut c: TtlSortedCache<u32, u32> = TtlSortedCache::builder()
                .ttl(Duration::from_millis(1))
                .build()
                .unwrap();
            c.cache_set(1u32, 100u32);
            std::thread::sleep(std::time::Duration::from_millis(20));
            c
        }

        /// Same as `expired_cache`, but wired with an `on_evict` that appends to a shared
        /// log, so the Ok-outcome sub-block below can pin *when* the displaced expired entry
        /// is evicted relative to the factory, not just the net counts. This is the
        /// discriminator the plain count assertions below cannot see: reverting the fix-2
        /// liveness check to an eager `cache_get`-based sweep still nets `cache_size() == 1`
        /// and `cache_evictions() == Some(1)` for the Ok outcome (the eager sweep evicts, then
        /// the factory's value is inserted fresh), so only the callback order and `cache_get`
        /// on-hit accounting expose that regression on this outcome.
        type Log = std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>;
        fn expired_cache_with_log() -> (TtlSortedCache<u32, u32>, Log) {
            let log: Log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let log2 = log.clone();
            let mut c: TtlSortedCache<u32, u32> = TtlSortedCache::builder()
                .ttl(Duration::from_millis(1))
                .on_evict(move |_k: &u32, _v: &u32| log2.lock().unwrap().push("evict"))
                .build()
                .unwrap();
            c.cache_set(1u32, 100u32);
            std::thread::sleep(std::time::Duration::from_millis(20));
            (c, log)
        }

        // --- Ok / infallible-success outcome: all four end up with the factory's value,
        // fire on_evict exactly once AFTER the factory runs, and count exactly one eviction. ---
        {
            let (mut sync_infallible, log) = expired_cache_with_log();
            let log2 = log.clone();
            let v = sync_infallible.cache_get_or_set_with_mut(1u32, || {
                log2.lock().unwrap().push("factory");
                200u32
            });
            assert_eq!(*v, 200u32);
            assert_eq!(sync_infallible.cache_size(), 1);
            assert_eq!(sync_infallible.cache_evictions(), Some(1));
            assert_eq!(
                *log.lock().unwrap(),
                vec!["factory", "evict"],
                "sync infallible: on_evict fires after the factory"
            );
            assert_index_lockstep(&sync_infallible, "sync infallible Ok outcome");

            let (mut sync_try, log) = expired_cache_with_log();
            let log2 = log.clone();
            let v: Result<&mut u32, &'static str> =
                sync_try.cache_try_get_or_set_with_mut(1u32, || {
                    log2.lock().unwrap().push("factory");
                    Ok(200u32)
                });
            assert_eq!(v.ok(), Some(&mut 200u32));
            assert_eq!(sync_try.cache_size(), 1);
            assert_eq!(sync_try.cache_evictions(), Some(1));
            assert_eq!(
                *log.lock().unwrap(),
                vec!["factory", "evict"],
                "sync try: on_evict fires after the factory"
            );
            assert_index_lockstep(&sync_try, "sync try Ok outcome");

            let (mut async_infallible, log) = expired_cache_with_log();
            let log2 = log.clone();
            let v = CachedGetOrSetAsync::async_cache_get_or_set_with_mut(
                &mut async_infallible,
                1u32,
                || async move {
                    log2.lock().unwrap().push("factory");
                    200u32
                },
            )
            .await;
            assert_eq!(*v, 200u32);
            assert_eq!(async_infallible.cache_size(), 1);
            assert_eq!(async_infallible.cache_evictions(), Some(1));
            assert_eq!(
                *log.lock().unwrap(),
                vec!["factory", "evict"],
                "async infallible: on_evict fires after the factory, not eagerly on the check"
            );
            assert_index_lockstep(&async_infallible, "async infallible Ok outcome");

            let (mut async_try, log) = expired_cache_with_log();
            let log2 = log.clone();
            let v: Result<&mut u32, &'static str> =
                CachedGetOrSetAsync::async_cache_try_get_or_set_with_mut(
                    &mut async_try,
                    1u32,
                    || async move {
                        log2.lock().unwrap().push("factory");
                        Ok(200u32)
                    },
                )
                .await;
            assert_eq!(v.ok(), Some(&mut 200u32));
            assert_eq!(
                *log.lock().unwrap(),
                vec!["factory", "evict"],
                "async try: on_evict fires after the factory, not eagerly on the check"
            );
            assert_eq!(async_try.cache_size(), 1);
            assert_eq!(async_try.cache_evictions(), Some(1));
            assert_index_lockstep(&async_try, "async try Ok outcome");
        }

        // --- Err outcome (fallible variants only): the expired entry is left exactly alone,
        // no eviction fires or is counted. ---
        {
            let mut sync_try = expired_cache();
            let v: Result<&mut u32, &'static str> =
                sync_try.cache_try_get_or_set_with_mut(1u32, || Err("nope"));
            assert_eq!(v.err(), Some("nope"));
            assert_eq!(sync_try.cache_size(), 1);
            assert_eq!(sync_try.cache_evictions(), Some(0));
            assert_index_lockstep(&sync_try, "sync try Err outcome");

            let mut async_try = expired_cache();
            let v: Result<&mut u32, &'static str> =
                CachedGetOrSetAsync::async_cache_try_get_or_set_with_mut(
                    &mut async_try,
                    1u32,
                    || async { Err("nope") },
                )
                .await;
            assert_eq!(v.err(), Some("nope"));
            assert_eq!(async_try.cache_size(), 1);
            assert_eq!(async_try.cache_evictions(), Some(0));
            assert_index_lockstep(&async_try, "async try Err outcome");
        }

        // --- Cancellation (async variants only): dropping the future mid-await leaves the
        // expired entry exactly alone, no eviction fires or is counted. ---
        {
            let mut async_infallible = expired_cache();
            {
                let mut fut = Box::pin(CachedGetOrSetAsync::async_cache_get_or_set_with_mut(
                    &mut async_infallible,
                    1u32,
                    std::future::pending::<u32>,
                ));
                let waker = std::task::Waker::noop();
                let mut cx = std::task::Context::from_waker(waker);
                assert!(matches!(
                    std::future::Future::poll(fut.as_mut(), &mut cx),
                    std::task::Poll::Pending
                ));
            }
            assert_eq!(async_infallible.cache_size(), 1);
            assert_eq!(async_infallible.cache_evictions(), Some(0));
            assert_index_lockstep(&async_infallible, "async infallible cancellation");

            let mut async_try = expired_cache();
            {
                let mut fut = Box::pin(CachedGetOrSetAsync::async_cache_try_get_or_set_with_mut::<
                    _,
                    _,
                    &'static str,
                >(&mut async_try, 1u32, || {
                    std::future::pending::<Result<u32, &'static str>>()
                }));
                let waker = std::task::Waker::noop();
                let mut cx = std::task::Context::from_waker(waker);
                assert!(matches!(
                    std::future::Future::poll(fut.as_mut(), &mut cx),
                    std::task::Poll::Pending
                ));
            }
            assert_eq!(async_try.cache_size(), 1);
            assert_eq!(async_try.cache_evictions(), Some(0));
            assert_index_lockstep(&async_try, "async try cancellation");
        }
    }
}
