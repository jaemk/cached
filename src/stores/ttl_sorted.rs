use crate::time::Duration;
use crate::time::Instant;
use crate::{CacheEvict, CacheTtl, Cached, CachedIter, CachedPeek, CachedRead, CloneCached};

use super::{DefaultHashBuilder, StripedCounter};
use std::borrow::Borrow;
use std::cmp::Ordering as CmpOrdering;
use std::collections::BTreeSet;
use std::hash::{BuildHasher, Hash, Hasher};
use std::ops::Bound::{Excluded, Included};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
#[cfg(feature = "async_core")]
use {super::CachedAsync, std::future::Future};

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

    fn is_expired(&self) -> bool {
        self.expiry.is_some_and(|e| e < Instant::now())
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

/// Policy for [`TtlSortedCache::insert_inner`] when `now + ttl` overflows `Instant`.
#[derive(Clone, Copy)]
enum TtlOverflow {
    /// Return [`super::CacheSetError::TimeBounds`] without mutating the cache.
    Error,
    /// Saturate the expiry to "now" (immediately stale) and still store the entry.
    SaturateNow,
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

    pub(crate) ttl: Duration,
    pub(crate) size_limit: Option<usize>,
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

impl<K, V, S> TtlSortedCacheBuilder<K, V, S> {
    /// Set the maximum number of entries (eviction bound). When the cache exceeds this
    /// limit, the next-to-expire entries are evicted until it is within bounds. Unlike
    /// [`capacity`](Self::capacity), this is a hard cap on entry count, not a preallocation
    /// hint.
    #[doc(alias = "size")]
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
    /// the backing map to `max_size + 1`, overriding a smaller `capacity` set here.
    #[must_use]
    pub fn capacity(mut self, capacity: usize) -> Self {
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
    /// use cached::stores::TtlSortedCache;
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
        let preallocate = self
            .capacity
            .or_else(|| self.size.map(|size| size.saturating_add(1)));
        if let Some(amount) = preallocate {
            cache.map.reserve(amount);
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

    /// Return a builder for constructing an [`TtlSortedCache`].
    #[must_use]
    pub fn builder() -> TtlSortedCacheBuilder<K, V> {
        TtlSortedCacheBuilder::default()
    }
}

impl<K: Hash + Eq + Ord + Clone, V, S: BuildHasher> TtlSortedCache<K, V, S> {
    /// Set the maximum number of entries. When reached, the next entries to expire are evicted.
    /// Returns the previous value if one was set.
    ///
    /// This grows the backing map to hold at least `max_size + 1` entries, so calling it on a
    /// cache built with a deliberately small [`capacity`](TtlSortedCacheBuilder::capacity) will
    /// override that smaller allocation.
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
        self.map.reserve(
            max_size
                .saturating_add(1)
                .saturating_sub(self.map.capacity()),
        );
        prev
    }

    /// Set a non-zero maximum number of entries. When reached, the next entries to expire are evicted.
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

    /// Increase backing stores with enough capacity to store `more`
    pub fn reserve(&mut self, more: usize) {
        self.map.reserve(more);
    }

    /// Set the default ttl and return the previous value.
    ///
    /// Returns `Some(previous_ttl)` to match [`CacheTtl::set_ttl`](crate::CacheTtl::set_ttl)
    /// and the `set_ttl` of every other timed store, so the return type is consistent
    /// regardless of which store a generic caller is using.
    pub fn set_ttl(&mut self, ttl: Duration) -> Option<Duration> {
        let prev = self.ttl;
        self.ttl = ttl;
        Some(prev)
    }

    /// Evict values that have expired.
    /// Returns number of dropped items.
    #[must_use]
    pub fn evict(&mut self) -> usize {
        let cutoff = Instant::now();
        let min = Stamped::bound(self.min_instant);
        let max = Stamped::bound(cutoff);
        let min = Included(&min);
        let max = Excluded(&max);
        let remove = self.keys.range((min, max)).count();

        let mut count = 0;
        while count < remove {
            match self.keys.pop_first() {
                None => break,
                Some(stamped) => {
                    // Invariant: `None` keys are only used as artificial range sentinels
                    // in `evict()`/`retain_latest()` and are never inserted into `self.keys`.
                    let key = stamped
                        .key
                        .expect("evicting: only artificial bounds are none");
                    if let Some(entry) = self.map.remove(key.0.as_ref()) {
                        if let Some(on_evict) = &self.on_evict {
                            on_evict(key.0.as_ref(), &entry.value);
                        }
                        self.evictions.fetch_add(1, AtomicOrdering::Relaxed);
                    }
                    count += 1;
                }
            }
        }
        count
    }

    /// Retain only the latest `count` values, dropping the next values to expire.
    /// If `evict`, then also evict values that have expired.
    /// Returns number of dropped items.
    pub fn retain_latest(&mut self, count: usize, evict: bool) -> usize {
        let retain_drop_count = self.map.len().saturating_sub(count);

        let remove = if evict {
            let cutoff = Instant::now();
            let min = Stamped::bound(self.min_instant);
            let max = Stamped::bound(cutoff);
            let min = Included(&min);
            let max = Excluded(&max);
            let to_evict_count = self.keys.range((min, max)).count();
            retain_drop_count.max(to_evict_count)
        } else {
            retain_drop_count
        };

        let mut count = 0;
        while count < remove {
            match self.keys.pop_first() {
                None => break,
                Some(stamped) => {
                    // Invariant: same as evict() — None keys are sentinel-only.
                    let key = stamped
                        .key
                        .expect("retaining: only artificial bounds are none");
                    if let Some(entry) = self.map.remove(key.0.as_ref()) {
                        if let Some(on_evict) = &self.on_evict {
                            on_evict(key.0.as_ref(), &entry.value);
                        }
                        self.evictions.fetch_add(1, AtomicOrdering::Relaxed);
                    }
                    count += 1;
                }
            }
        }
        count
    }

    /// Insert k/v pair without running eviction logic. See `.insert_ttl_evict`
    pub fn insert(&mut self, key: K, value: V) -> Result<Option<V>, super::CacheSetError> {
        self.insert_ttl_evict(key, value, None, false)
    }

    /// Insert k/v pair with explicit ttl. See `.insert_ttl_evict`
    pub fn insert_ttl(
        &mut self,
        key: K,
        value: V,
        ttl: Duration,
    ) -> Result<Option<V>, super::CacheSetError> {
        self.insert_ttl_evict(key, value, Some(ttl), false)
    }

    /// Insert k/v pair and run eviction logic. See `.insert_ttl_evict`
    pub fn insert_evict(
        &mut self,
        key: K,
        value: V,
        evict: bool,
    ) -> Result<Option<V>, super::CacheSetError> {
        self.insert_ttl_evict(key, value, None, evict)
    }

    /// Insert a k/v pair with an optional explicit TTL, then optionally run eviction logic.
    /// The entry is inserted first. If a `size_limit` was specified and capacity is exceeded,
    /// the next-to-expire entry is dropped after insertion. The eviction callback fires after
    /// insertion, not before. Returns any existing unexpired value that was replaced.
    pub fn insert_ttl_evict(
        &mut self,
        key: K,
        value: V,
        ttl: Option<Duration>,
        evict: bool,
    ) -> Result<Option<V>, super::CacheSetError> {
        self.insert_inner(key, value, ttl, evict, TtlOverflow::Error, false)
    }

    /// Shared insertion routine for [`insert_ttl_evict`](Self::insert_ttl_evict) and the
    /// infallible `cache_get_or_set_with_mut` paths.
    ///
    /// `on_overflow` selects what happens in the (practically unreachable) case where
    /// `now + ttl` exceeds `Instant`'s representable range — a TTL on the order of
    /// hundreds of years:
    /// - [`TtlOverflow::Error`]: return [`super::CacheSetError::TimeBounds`] before any mutation
    ///   (used by the fallible public API).
    /// - [`TtlOverflow::SaturateNow`]: store the entry with an already-elapsed expiry
    ///   so the value is still retained (and returnable by reference) but is treated as
    ///   immediately stale. Size-limit enforcement is skipped in this branch so the
    ///   just-inserted entry cannot be the one evicted, which lets the infallible
    ///   `get_or_set` paths return `&mut V` without a fallible re-lookup.
    ///
    /// When the effective TTL (explicit `ttl` arg or `self.ttl`) is zero, the entry is
    /// stored with `expiry = None` (never expires) rather than being given an immediate
    /// expiry. Zero TTL means "disable expiry" for new inserts, consistent with the other
    /// TTL stores. In this case `overflowed` is always `false`.
    fn insert_inner(
        &mut self,
        key: K,
        value: V,
        ttl: Option<Duration>,
        evict: bool,
        on_overflow: TtlOverflow,
        skip_size_eviction: bool,
    ) -> Result<Option<V>, super::CacheSetError> {
        let arc_key = CacheArc::new(key.clone());
        let effective_ttl = ttl.unwrap_or(self.ttl);

        // A zero TTL means "never expires": store expiry = None.
        let (expiry, overflowed) = if effective_ttl.is_zero() {
            (None, false)
        } else {
            let now = Instant::now();
            match now.checked_add(effective_ttl) {
                Some(t) => (Some(t), false),
                None => match on_overflow {
                    TtlOverflow::Error => return Err(super::CacheSetError::TimeBounds),
                    TtlOverflow::SaturateNow => (Some(now), true),
                },
            }
        };

        let new_stamped = Stamped {
            expiry,
            key: Some(arc_key.clone()),
        };
        self.keys.insert(new_stamped.clone());
        let old = self.map.insert(
            key,
            Entry {
                expiry,
                key: arc_key,
                value,
            },
        );
        if let Some(old) = &old {
            let old_stamped = old.as_stamped();
            if old_stamped != new_stamped {
                self.keys.remove(&old_stamped);
            }
        }
        let old_value = old.and_then(|entry| {
            if entry.is_expired() {
                None
            } else {
                Some(entry.value)
            }
        });

        // Skip size-limit eviction in two cases:
        // 1. The TTL overflowed and was saturated to `now` — the new entry has the earliest
        //    possible expiry and would be the first thing `retain_latest` drops.
        // 2. The caller explicitly requests it (`skip_size_eviction`) — e.g. `set_and_get_mut`
        //    must guarantee the just-inserted entry is still present to return `&mut V` safely,
        //    regardless of the entry's TTL.
        if !overflowed && !skip_size_eviction {
            if let Some(size_limit) = self.size_limit {
                if self.map.len() > size_limit {
                    self.retain_latest(size_limit, evict);
                }
            } else if evict {
                let _ = self.evict();
            }
        }

        Ok(old_value)
    }

    /// Insert `key`/`value` and return a mutable reference to the stored value.
    ///
    /// Unlike [`insert`](Self::insert) this never fails and never drops the value:
    /// an unrepresentable TTL saturates to an immediately-stale entry rather than
    /// erroring. When a `size_limit` is configured the just-inserted entry is
    /// protected from eviction: other entries are evicted in TTL order to restore
    /// capacity. Used by the infallible `cache_get_or_set_with_mut` family.
    fn set_and_get_mut(&mut self, key: K, value: V) -> &mut V {
        // `Ok` is guaranteed: `TtlOverflow::SaturateNow` never returns `Err`.
        // `skip_size_eviction = true` defers size enforcement to the block below,
        // where we can protect the just-inserted entry.
        let _ = self.insert_inner(
            key.clone(),
            value,
            None,
            false,
            TtlOverflow::SaturateNow,
            true,
        );

        if let Some(size_limit) = self.size_limit
            && self.map.len() > size_limit
        {
            // Temporarily unlink the just-inserted entry from the expiry index so
            // `retain_latest` cannot select it for eviction. Other entries are
            // dropped in TTL order until the map is back within `size_limit`.
            // The stamp is restored afterward so the index stays consistent.
            let protected = self.map[&key].as_stamped();
            self.keys.remove(&protected);
            self.retain_latest(size_limit, false);
            // If the TTL overflowed (SaturateNow), protected.expiry == now —
            // the entry is immediately stale but the caller holds a live &mut V.
            self.keys.insert(protected);
        }

        &mut self
            .map
            .get_mut(&key)
            .expect(
                "set_and_get_mut: SaturateNow never errors and the protected eviction \
                 path guarantees the entry is present",
            )
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

impl<K: Hash + Eq + Ord + Clone, V, S: BuildHasher> Cached<K, V> for TtlSortedCache<K, V, S> {
    type Error = super::CacheSetError;

    fn cache_get<Q>(&mut self, key: &Q) -> Option<&V>
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
        // Silently treat an Instant overflow as a no-op; callers that need
        // to distinguish this case should use cache_try_set instead.
        self.insert(key, value).unwrap_or(None)
    }

    fn cache_try_set(&mut self, k: K, v: V) -> Result<Option<V>, super::CacheSetError> {
        self.insert(k, v)
    }

    fn cache_get_or_set_with_mut<F: FnOnce() -> V>(&mut self, key: K, f: F) -> &mut V {
        if self.cache_get(&key).is_some() {
            return self
                .map
                .get_mut(&key)
                .map(|entry| &mut entry.value)
                // Invariant: cache_get confirmed the entry exists and is not expired.
                // No other code path removes it between the check and this get_mut.
                .expect("cache entry vanished");
        }
        // `set_and_get_mut` never drops the value (it saturates an unrepresentable
        // TTL instead of erroring), so this path is panic-free.
        self.set_and_get_mut(key, f())
    }

    fn cache_try_get_or_set_with_mut<F: FnOnce() -> Result<V, E>, E>(
        &mut self,
        key: K,
        f: F,
    ) -> Result<&mut V, E> {
        if self.cache_get(&key).is_some() {
            return Ok(self
                .map
                .get_mut(&key)
                .map(|entry| &mut entry.value)
                // Invariant: same as cache_get_or_set_with above.
                .expect("cache entry vanished"));
        }
        // `set_and_get_mut` never drops the value, so this path is panic-free.
        Ok(self.set_and_get_mut(key, f()?))
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
                let stored_k = (*removed.key.0).clone();
                if let Some(on_evict) = &self.on_evict {
                    on_evict(&stored_k, &removed.value);
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
        self.map.clear();
        self.map.shrink_to(0);
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
}

impl<K: Hash + Eq + Ord, V, S: BuildHasher> CachedIter<K, V> for TtlSortedCache<K, V, S> {
    fn iter<'a>(&'a self) -> impl Iterator<Item = (&'a K, &'a V)> + 'a
    where
        K: 'a,
        V: 'a,
    {
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
    /// Returns `Some(ttl)` — the currently configured TTL duration.
    ///
    /// When `ttl` is `Duration::ZERO`, entries inserted while zero is set never expire
    /// (they are stored with `expiry = None`). This method still reports `Some(Duration::ZERO)`
    /// in that case so callers can observe the configured value.
    fn ttl(&self) -> Option<Duration> {
        Some(self.ttl)
    }
    /// Set the global TTL for future inserts, returning the previous value.
    ///
    /// A zero `Duration` disables expiry for **future** inserts: entries inserted while the TTL
    /// is zero are stored with `expiry = None` and never expire. Pre-existing entries keep their
    /// original expiry and still expire on schedule. This is consistent with the other TTL stores
    /// (`TtlCache`, `LruTtlCache`). To restore expiry, call `set_ttl` with a non-zero duration.
    fn set_ttl(&mut self, ttl: Duration) -> Option<Duration> {
        let prev = self.ttl;
        self.ttl = ttl;
        Some(prev)
    }
    /// Disable expiry for future inserts by setting the TTL to `Duration::ZERO`.
    ///
    /// Equivalent to `set_ttl(Duration::ZERO)`: entries inserted after this call never expire.
    /// Pre-existing entries keep their original expiry. Returns `None` (no "previous unset" state
    /// to restore; use `ttl()` to capture the previous value before calling `unset_ttl` if
    /// needed).
    fn unset_ttl(&mut self) -> Option<Duration> {
        self.ttl = Duration::ZERO;
        None
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
impl<K, V, S> CachedAsync<K, V> for TtlSortedCache<K, V, S>
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
            if self.cache_get(&k).is_some() {
                return self
                    .map
                    .get_mut(&k)
                    .map(|entry| &mut entry.value)
                    // Invariant: cache_get confirmed the entry is present and unexpired.
                    .expect("cache entry vanished");
            }
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
            if self.cache_get(&k).is_some() {
                return Ok(self
                    .map
                    .get_mut(&k)
                    .map(|entry| &mut entry.value)
                    // Invariant: cache_get confirmed the entry is present and unexpired.
                    .expect("cache entry vanished"));
            }
            // `set_and_get_mut` never drops the value, so this path is panic-free.
            let value = f().await?;
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
    use crate::{Cached, CachedExt, CachedRead};
    use std::cmp::Ordering as CmpOrdering;
    use std::hash::{Hash, Hasher};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn ttl_sorted_cache_set_error_is_clone_eq() {
        // TtlSortedCache now uses CacheSetError (unified with TtlCache / LruTtlCache).
        use crate::stores::CacheSetError;
        assert_eq!(CacheSetError::TimeBounds, CacheSetError::TimeBounds.clone());
    }

    #[test]
    fn ttl_sorted_cache_try_set_returns_cache_set_error_on_overflow() {
        // insert_ttl with a Duration that would overflow Instant bounds must return
        // CacheSetError::TimeBounds (no longer TtlSortedCacheError).
        use crate::stores::CacheSetError;
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        // Duration::MAX overflows Instant::now().checked_add -> None -> Error branch.
        let result = cache.insert_ttl(1u32, 42u32, Duration::MAX);
        assert_eq!(result, Err(CacheSetError::TimeBounds));
        // The cache must not be mutated on error.
        assert_eq!(cache.cache_size(), 0);
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
            .capacity(100)
            .build()
            .unwrap();
        cache.insert(String::from("a"), "a").unwrap();
        assert_eq!(cache.get("a").unwrap(), &"a");

        let mut cache = TtlSortedCache::builder()
            .ttl(Duration::from_millis(100))
            .capacity(100)
            .build()
            .unwrap();
        cache.insert(vec![0], "a").unwrap();
        assert_eq!(cache.get([0].as_slice()).unwrap(), &"a");
    }

    #[test]
    fn cache_get_live_hit_increments_hits() {
        let key = CountingKey::new("live");
        let mut cache = TtlSortedCache::builder()
            .ttl(Duration::from_secs(60))
            .capacity(1)
            .build()
            .unwrap();
        cache.insert(key.clone(), 10).unwrap();

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
            .capacity(1)
            .build()
            .unwrap();
        cache.insert(key.clone(), 10).unwrap();

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
            .insert_ttl("expired", 10, Duration::from_millis(1))
            .unwrap();
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
            .insert_ttl("expired-mut", 20, Duration::from_millis(1))
            .unwrap();
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
            .capacity(100)
            .build()
            .unwrap();
        assert_eq!(0, cache.evict());
        assert_eq!(0, cache.retain_latest(100, true));
        assert!(cache.get("a").is_none());

        cache.insert("a".to_string(), "A".to_string()).unwrap();
        assert_eq!(cache.get("a"), Some("A".to_string()).as_ref());
        assert_eq!(cache.len(), 1);
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(1, cache.evict());
        assert!(cache.get("a").is_none());
        assert_eq!(cache.len(), 0);

        cache.insert("a".to_string(), "A".to_string()).unwrap();
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

        cache.insert("a".to_string(), "a".to_string()).unwrap();
        cache.insert("b".to_string(), "b".to_string()).unwrap();
        cache.insert("c".to_string(), "c".to_string()).unwrap();
        cache.insert("d".to_string(), "d".to_string()).unwrap();
        cache.insert("e".to_string(), "e".to_string()).unwrap();
        assert_eq!(3, cache.retain_latest(2, false));
        assert_eq!(2, cache.len());
        assert_eq!(cache.get("a"), None);
        assert_eq!(cache.get("b"), None);
        assert_eq!(cache.get("c"), None);
        assert_eq!(cache.get("d"), Some("d".to_string()).as_ref());
        assert_eq!(cache.get("e"), Some("e".to_string()).as_ref());

        cache.insert("a".to_string(), "a".to_string()).unwrap();
        cache.insert("a".to_string(), "a".to_string()).unwrap();
        cache.insert("b".to_string(), "b".to_string()).unwrap();
        cache.insert("b".to_string(), "b".to_string()).unwrap();
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

        cache.insert("a".to_string(), "a".to_string()).unwrap();
        assert_eq!(cache.remove("a"), Some("a".to_string()));
        // we haven't done anything to evict "b" so there's still one entry
        assert_eq!(1, cache.len());

        assert_eq!(1, cache.evict());
        assert_eq!(0, cache.len());

        // default ttl is 100ms
        cache
            .insert_ttl("a".to_string(), "a".to_string(), Duration::from_millis(300))
            .unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(cache.get("a"), Some("a".to_string()).as_ref());
        assert_eq!(1, cache.len());

        std::thread::sleep(Duration::from_millis(200));
        cache
            .insert_ttl_evict(
                "b".to_string(),
                "b".to_string(),
                Some(Duration::from_millis(300)),
                true,
            )
            .unwrap();
        // a should now be evicted
        assert_eq!(1, cache.len());
        assert_eq!(cache.get("a"), None);
    }

    #[test]
    fn set_max_size() {
        let mut cache = TtlSortedCache::builder()
            .ttl(Duration::from_millis(100))
            .capacity(100)
            .build()
            .unwrap();
        cache.set_max_size(2);
        assert_eq!(0, cache.evict());
        assert_eq!(0, cache.retain_latest(100, true));
        assert!(cache.get("a").is_none());

        cache.insert("a".to_string(), "A".to_string()).unwrap();
        assert_eq!(cache.get("a"), Some("A".to_string()).as_ref());
        assert_eq!(cache.len(), 1);
        cache.insert("b".to_string(), "B".to_string()).unwrap();
        assert_eq!(cache.get("b"), Some("B".to_string()).as_ref());
        assert_eq!(cache.len(), 2);
        cache.insert("c".to_string(), "C".to_string()).unwrap();
        assert_eq!(cache.len(), 2);
        assert_eq!(cache.get("b"), Some("B".to_string()).as_ref());
        assert_eq!(cache.get("c"), Some("C".to_string()).as_ref());
        assert_eq!(cache.get("a"), None);
    }

    #[test]
    fn updating_existing_key_at_size_limit_does_not_evict_another_key() {
        let mut cache = TtlSortedCache::builder()
            .ttl(Duration::from_millis(1_000))
            .capacity(2)
            .build()
            .unwrap();
        cache.set_max_size(2);

        cache.insert("a".to_string(), "A".to_string()).unwrap();
        cache.insert("b".to_string(), "B".to_string()).unwrap();
        assert_eq!(cache.len(), 2);

        assert_eq!(
            cache.insert("a".to_string(), "A2".to_string()).unwrap(),
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
            Err(super::super::SetMaxSizeError::ZeroSize)
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

    #[test]
    fn explicit_capacity_takes_precedence_over_max_size_preallocation() {
        // Regression for #266: an explicit, smaller `capacity` must not be defeated
        // by `max_size`'s `size + 1` preallocation (HashMap::reserve does not reduce
        // an existing allocation).
        let cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(300))
            .max_size(65_536)
            .capacity(16)
            .build()
            .unwrap();
        // The backing map must not have taken the max_size path, which would reserve
        // for max_size + 1 (= 65_537) entries.
        assert!(
            cache.map.capacity() < 65_537,
            "expected the explicit capacity(16) to take precedence, got {}",
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
            .insert_ttl("long", 1u32, Duration::from_secs(60))
            .unwrap();
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
            .insert_ttl("long", 1u32, Duration::from_secs(60))
            .unwrap();
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
        use crate::CachedAsync;
        let mut cache = TtlSortedCache::builder()
            .ttl(Duration::from_millis(1))
            .build()
            .unwrap();
        cache.set_max_size(1);
        cache
            .insert_ttl("long", 1u32, Duration::from_secs(60))
            .unwrap();
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
        // ttl() still reports the configured value.
        assert_eq!(CacheTtl::ttl(&cache), Some(Duration::ZERO));
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
            Some(Duration::ZERO),
            "unset_ttl sets internal ttl to zero"
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

    /// `insert_ttl` called with an EXPLICIT `Duration::ZERO` (not the cache-level
    /// `set_ttl`) must store `expiry = None` (never expires), not `Some(now)`
    /// (immediate). The cache's default TTL stays finite the whole time.
    #[test]
    fn insert_ttl_explicit_zero_never_expires() {
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_millis(20))
            .build()
            .unwrap();
        // Explicit zero TTL on this one entry — default ttl remains 20ms.
        cache.insert_ttl(1u32, 10u32, Duration::ZERO).unwrap();
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

    /// `insert_ttl_evict` with explicit `Duration::ZERO` also stores `None`,
    /// and the never-expiring entry is not swept by the eviction pass it triggers.
    #[test]
    fn insert_ttl_evict_explicit_zero_never_expires_and_survives_evict() {
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_millis(10))
            .build()
            .unwrap();
        // A finite, soon-to-expire entry.
        cache.cache_set(1u32, 10u32);
        std::thread::sleep(std::time::Duration::from_millis(40));
        // Insert a never-expiring entry AND run the eviction pass in the same call.
        cache
            .insert_ttl_evict(2u32, 20u32, Some(Duration::ZERO), true)
            .unwrap();
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

    /// The four renamed single-owner `CachedAsync` default methods, exercised on a real
    /// `TtlSortedCache` (not only `UnboundCache`). Confirms the rename works on a store
    /// whose `cache_get`/`cache_set`/`cache_remove`/`cache_clear` carry TTL semantics.
    #[cfg(feature = "async")]
    #[tokio::test]
    async fn async_cache_methods_on_ttl_sorted_cache() {
        use crate::CachedAsync;
        let mut cache: TtlSortedCache<String, u32> = TtlSortedCache::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();

        let prev = cache.async_cache_set("a".to_string(), 1u32).await;
        assert_eq!(prev, None, "first insert returns None");

        let prev = cache.async_cache_set("a".to_string(), 2u32).await;
        assert_eq!(prev, Some(1u32), "overwrite returns previous value");

        let got = cache.async_cache_get("a").await;
        assert_eq!(got, Some(&2u32), "async_cache_get hit");

        let missing = cache.async_cache_get("z").await;
        assert_eq!(missing, None, "async_cache_get miss");

        let removed = cache.async_cache_remove("a").await;
        assert_eq!(removed, Some(2u32), "async_cache_remove returns value");
        assert_eq!(cache.async_cache_get("a").await, None, "gone after remove");

        cache.async_cache_set("x".to_string(), 10u32).await;
        cache.async_cache_set("y".to_string(), 20u32).await;
        assert_eq!(cache.cache_size(), 2);
        cache.async_cache_clear().await;
        assert_eq!(cache.cache_size(), 0, "async_cache_clear empties cache");
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
}
