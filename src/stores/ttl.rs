use crate::time::Duration;
use crate::time::Instant;
use std::cmp::Eq;
use std::hash::{BuildHasher, Hash};

use std::collections::{HashMap, hash_map::Entry};

#[cfg(feature = "async_core")]
use {super::CachedGetOrSetAsync, std::future::Future};

use crate::{CacheExpiry, CachedIter, CachedPeek, CloneCached};

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

impl<K, V> TtlCacheBuilder<K, V> {
    /// Create a builder with default settings. Equivalent to [`TtlCache::builder`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
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

    /// Same as [`entry_live`](Self::entry_live) but takes an already-sampled `now`
    /// instead of reading the clock. Lets hot paths that already have `now` in hand
    /// (e.g. a caller that just computed a fresh expiry) avoid a redundant clock read.
    #[inline]
    pub(super) fn entry_live_at(expires_at: Option<Instant>, now: Instant) -> bool {
        expires_at.is_none_or(|t| now < t)
    }

    /// Insert `entry` for `key`, returning the previous value only if it was still live.
    ///
    /// When the displaced previous value had already expired it is filtered from the return
    /// (matching the get paths), so it is dropped silently from the caller's view; in that case
    /// fire `on_evict` and count an eviction so resource cleanup and metrics stay consistent
    /// with the other removal paths.
    ///
    /// `now` is the caller's already-sampled clock reading, used to decide whether the
    /// displaced entry was still live -- avoids a second `Instant::now()` call here.
    fn set_entry(&mut self, key: K, entry: TimedEntry<V>, now: Instant) -> Option<V> {
        use std::collections::hash_map::Entry;
        match self.store.entry(key) {
            Entry::Occupied(mut occupied) => {
                let old = occupied.insert(entry);
                if Self::entry_live_at(old.expires_at, now) {
                    Some(old.value)
                } else {
                    // Count BEFORE notifying: a panicking callback must never leave
                    // an entry removed-but-uncounted.
                    self.evictions.fetch_add(1, Ordering::Relaxed);
                    if let Some(on_evict) = &self.on_evict {
                        on_evict(occupied.key(), &old.value);
                    }
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

    /// Phase 1 of a two-phase sweep: run `doomed` over every entry and hand back the
    /// entries it selected, removed from the store.
    ///
    /// See [`take_doomed`](crate::stores::take_doomed) for why the sweep is split in two and
    /// what ties the passes together.
    fn take_doomed<F: FnMut(&K, &TimedEntry<V>) -> bool>(
        &mut self,
        doomed: F,
    ) -> Vec<(K, TimedEntry<V>)> {
        crate::stores::take_doomed(&mut self.store, doomed)
    }

    /// Phase 2 of a two-phase sweep: count `removed` as evictions and then notify
    /// `on_evict` for each, returning how many entries were removed.
    ///
    /// The entries are already out of the store, and the whole batch is counted before the
    /// first notification, so a panicking `on_evict` can never leave an entry that has been
    /// cleaned up still reachable, nor an entry removed-but-uncounted.
    fn notify_evicted(&self, removed: &[(K, TimedEntry<V>)]) -> usize {
        if !removed.is_empty() {
            self.evictions
                .fetch_add(removed.len() as u64, Ordering::Relaxed);
        }
        if let Some(on_evict) = &self.on_evict {
            for (k, entry) in removed {
                on_evict(k, &entry.value);
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
        let now = Instant::now();
        // Two-phase: select, then remove, then count, then notify. Counting or notifying
        // from inside a `HashMap::retain` predicate would fire the side effects *before*
        // the map drops the entry, so a panicking `on_evict` would leave an entry counted
        // (and cleaned up) while still stored and served.
        // None means never-expires; Some(t) expires when now >= t.
        let removed = self.take_doomed(|_key, entry| !Self::entry_live_at(entry.expires_at, now));
        self.notify_evicted(&removed)
    }

    /// Retain only entries that are unexpired and satisfy `keep`.
    ///
    /// Removes every entry that is already TTL-expired **or** for which `keep`
    /// returns `false` — expired entries are removed without consulting `keep`.
    /// `on_evict` is called and the eviction counter incremented for each removed
    /// entry. This matches [`LruTtlCache::retain`](crate::LruTtlCache::retain) and
    /// [`ExpiringLruCache::retain`](crate::ExpiringLruCache::retain); the plain
    /// [`LruCache::retain`](crate::LruCache::retain) has no expiry dimension and
    /// removes solely on the predicate.
    ///
    /// Returns the number of entries removed: the count folds together entries `keep`
    /// rejected and entries swept for having already expired, since expiry removal is
    /// unconditional regardless of what `keep` returns. `retain` is deliberately not
    /// `#[must_use]`: discarding the count is a legitimate and common use, matching
    /// existing bare `cache.retain(...);` call sites.
    pub fn retain<F: FnMut(&K, &V) -> bool>(&mut self, mut keep: F) -> usize {
        // Sample the clock once for the whole eager sweep, as `evict` does above --
        // one `now` shared across every entry instead of a clock read per entry.
        let now = Instant::now();
        // Two-phase (see `take_doomed`): the selection pass must be side-effect free so a
        // panicking `keep` leaves the cache untouched rather than half-notified.
        let removed = self.take_doomed(|key, entry| {
            let expired = !Self::entry_live_at(entry.expires_at, now);
            expired || !keep(key, &entry.value)
        });
        self.notify_evicted(&removed)
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
        let now = Instant::now();
        let expired_present = match self.store.get_mut(key) {
            Some(entry) if Self::entry_live_at(entry.expires_at, now) => {
                self.hits.fetch_add(1, Ordering::Relaxed);
                if self.refresh {
                    entry.expires_at = Self::refreshed_expires_at(self.ttl, now, entry.expires_at);
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
            // Count BEFORE notifying: a panicking callback must never leave
            // an entry removed-but-uncounted.
            self.evictions.fetch_add(1, Ordering::Relaxed);
            if let Some(on_evict) = &self.on_evict {
                on_evict(&k, &entry.value);
            }
        }
        None
    }

    fn cache_get_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        // Single lookup on the miss path, as in `cache_get` (CORE-7).
        let now = Instant::now();
        let expired_present = match self.store.get_mut(key) {
            Some(entry) if Self::entry_live_at(entry.expires_at, now) => {
                self.hits.fetch_add(1, Ordering::Relaxed);
                if self.refresh {
                    entry.expires_at = Self::refreshed_expires_at(self.ttl, now, entry.expires_at);
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
            // Count BEFORE notifying: a panicking callback must never leave
            // an entry removed-but-uncounted.
            self.evictions.fetch_add(1, Ordering::Relaxed);
            if let Some(on_evict) = &self.on_evict {
                on_evict(&k, &entry.value);
            }
        }
        None
    }

    fn cache_get_or_set_with_mut<F: FnOnce() -> V>(&mut self, key: K, f: F) -> &mut V {
        match self.store.entry(key) {
            Entry::Occupied(mut occupied) => {
                // Sample once and reuse for both the liveness check and the refresh
                // computation below -- avoids a second clock read on the hit path.
                let now = Instant::now();
                if Self::entry_live_at(occupied.get().expires_at, now) {
                    if self.refresh {
                        let new_exp =
                            Self::refreshed_expires_at(self.ttl, now, occupied.get().expires_at);
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
                    let now = Instant::now();
                    let expires_at = Self::compute_expires_at(self.ttl, now);
                    // Replace FIRST, then count, then notify -- as `set_entry` does. Firing
                    // the side effects while the expired entry is still installed would let a
                    // panicking `on_evict` leave it in place *and* counted, so the retry that
                    // finally replaces it counts a second eviction for one physical entry.
                    let old = occupied.insert(TimedEntry {
                        expires_at,
                        value: val,
                    });
                    self.evictions.fetch_add(1, Ordering::Relaxed);
                    if let Some(on_evict) = &self.on_evict {
                        on_evict(occupied.key(), &old.value);
                    }
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
                // Sample once and reuse for both the liveness check and the refresh
                // computation below -- avoids a second clock read on the hit path.
                let now = Instant::now();
                if Self::entry_live_at(occupied.get().expires_at, now) {
                    if self.refresh {
                        let new_exp =
                            Self::refreshed_expires_at(self.ttl, now, occupied.get().expires_at);
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
                    let now = Instant::now();
                    let expires_at = Self::compute_expires_at(self.ttl, now);
                    // Replace FIRST, then count, then notify -- see
                    // `cache_get_or_set_with_mut` above.
                    let old = occupied.insert(TimedEntry {
                        expires_at,
                        value: val,
                    });
                    self.evictions.fetch_add(1, Ordering::Relaxed);
                    if let Some(on_evict) = &self.on_evict {
                        on_evict(occupied.key(), &old.value);
                    }
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
    /// with `expires_at = None` (never expires). The sharded TTL stores clamp instead,
    /// so they differ here; see `compute_expires_at`.
    fn cache_set(&mut self, key: K, val: V) -> Option<V> {
        let now = Instant::now();
        let expires_at = Self::compute_expires_at(self.ttl, now);
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
        if let Some((stored_k, entry)) = self.store.remove_entry(k) {
            // Judge liveness at the moment of removal, BEFORE the callback runs. Sampling
            // it afterwards would let a slow `on_evict` push the entry past its deadline and
            // report `None` for a value that was live when it was taken out.
            let live = Self::entry_live(entry.expires_at);
            // Count BEFORE notifying: a panicking callback must never leave
            // an entry removed-but-uncounted.
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
        if let Some((stored_k, entry)) = self.store.remove_entry(k) {
            // Count BEFORE notifying: a panicking callback must never leave
            // an entry removed-but-uncounted.
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
        // Deliberately NOT hoisted (unlike the eager `retain`/`evict` sweeps above):
        // this iterator is lazy and may be held and advanced over an arbitrary span
        // of wall-clock time, so each item's liveness must be judged against a clock
        // read taken at the moment that item is produced, not a single snapshot from
        // when `iter()` was called.
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
}

impl<K: Hash + Eq, V, S: BuildHasher> crate::CacheRefreshOnHit for TtlCache<K, V, S> {
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
            let now = Instant::now();
            let expired = !Self::entry_live_at(entry.expires_at, now);
            if expired {
                self.misses.fetch_add(1, Ordering::Relaxed);
                (Some(entry.value.clone()), true)
            } else {
                self.hits.fetch_add(1, Ordering::Relaxed);
                if self.refresh {
                    let new_exp = Self::refreshed_expires_at(self.ttl, now, entry.expires_at);
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

impl<K: Hash + Eq, V, S: BuildHasher> CacheExpiry<K, V> for TtlCache<K, V, S> {
    /// Returns the stored value and its expiry instant, with no read side effects.
    ///
    /// The instant is the entry's own deadline, `None` when the entry never expires (TTL was
    /// disabled at insert time). `None` also when `now + ttl` overflowed `Instant` at insert
    /// time, so no deadline could be recorded. An expired entry is returned with its past
    /// deadline and is **not** removed. Uses the same lookup as
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
        if let Some(entry) = self.store.get(k) {
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
        match self.store.get(k) {
            Some(entry) => (true, entry.expires_at),
            None => (false, None),
        }
    }
}

#[cfg(feature = "async_core")]
#[cfg_attr(docsrs, doc(cfg(feature = "async_core")))]
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
            // One clock sample serves both the liveness check and the refresh
            // recompute; the miss branch re-samples after the factory (CORE-3).
            let now = Instant::now();
            match self.store.entry(k) {
                Entry::Occupied(mut occupied) => {
                    if Self::entry_live_at(occupied.get().expires_at, now) {
                        if self.refresh {
                            let new_exp = Self::refreshed_expires_at(
                                self.ttl,
                                now,
                                occupied.get().expires_at,
                            );
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
                        let now = Instant::now();
                        let expires_at = Self::compute_expires_at(self.ttl, now);
                        // Replace FIRST, then count, then notify -- see the sync
                        // `cache_get_or_set_with_mut`.
                        let old = occupied.insert(TimedEntry {
                            expires_at,
                            value: val,
                        });
                        self.evictions.fetch_add(1, Ordering::Relaxed);
                        if let Some(on_evict) = &self.on_evict {
                            on_evict(occupied.key(), &old.value);
                        }
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
            // One clock sample serves both the liveness check and the refresh
            // recompute; the miss branch re-samples after the factory (CORE-3).
            let now = Instant::now();
            let v = match self.store.entry(k) {
                Entry::Occupied(mut occupied) => {
                    if Self::entry_live_at(occupied.get().expires_at, now) {
                        if self.refresh {
                            let new_exp = Self::refreshed_expires_at(
                                self.ttl,
                                now,
                                occupied.get().expires_at,
                            );
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
                        let now = Instant::now();
                        let expires_at = Self::compute_expires_at(self.ttl, now);
                        // Replace FIRST, then count, then notify -- see the sync
                        // `cache_get_or_set_with_mut`.
                        let old = occupied.insert(TimedEntry {
                            expires_at,
                            value: val,
                        });
                        self.evictions.fetch_add(1, Ordering::Relaxed);
                        if let Some(on_evict) = &self.on_evict {
                            on_evict(occupied.key(), &old.value);
                        }
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

impl<K: Hash + Eq, V, S: BuildHasher> crate::CacheClearWithOnEvict for TtlCache<K, V, S> {
    fn cache_clear_with_on_evict(&mut self) {
        TtlCache::cache_clear_with_on_evict(self);
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
        let seen = Arc::new(std::sync::Mutex::new(Vec::<u32>::new()));
        let seen2 = seen.clone();
        let mut c: TtlCache<CoarseKey, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_secs(60))
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

        c.cache_set(first, 10);
        assert_eq!(c.cache_set(second.clone(), 20), Some(10));
        assert_eq!(c.cache_size(), 1);

        let (stored, value) = c.cache_remove_entry(&second).expect("present");
        assert_eq!(
            stored.payload, 1,
            "an overwrite keeps the incumbent key, so the first payload is the stored one"
        );
        assert_eq!(value, 20);
        assert_eq!(
            *seen.lock().unwrap(),
            vec![1u32],
            "the removal callback receives the stored key"
        );
    }

    /// Same fast path when the displaced entry had expired: the value is replaced in place,
    /// the eviction is counted and `on_evict` fires with the stored key -- and the key that
    /// remains stored is still the first one.
    #[test]
    fn cache_set_over_expired_keeps_the_first_stored_key() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::<u32>::new()));
        let seen2 = seen.clone();
        let mut c: TtlCache<CoarseKey, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_millis(20))
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
        c.cache_set(first, 10);
        std::thread::sleep(std::time::Duration::from_millis(60));

        assert_eq!(c.cache_set(second.clone(), 20), None, "displaced expired");
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
        assert_eq!(value, 20);
    }

    #[test]
    fn cache_set_with_ttl_overflow_stores_never_expiring_entry() {
        // A TTL that would overflow Instant bounds (compute_expires_at's
        // now.checked_add(ttl) -> None) stores the entry with no expiry: it never
        // expires, matching TtlSortedCache's set_with(..).ttl(..) overflow behavior.
        use crate::CacheTtl;
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_secs(60))
            .build()
            .unwrap();
        c.set_ttl(crate::time::Duration::MAX);
        assert_eq!(c.cache_set(1, 42), None);
        assert_eq!(c.cache_get(&1), Some(&42));
        // Never-expiring: cache_peek_with_expiry_status must not report it as expired.
        assert_eq!(c.cache_peek_with_expiry_status(&1), (Some(42), false));
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

    // PERF-1: `entry_live_at` must preserve the exact same `now >= expires_at`
    // boundary convention as `entry_live` (which reads `Instant::now()` internally).
    // At `now == expires_at` the entry must be considered expired, mirroring
    // `entry_live`'s strict `now < t` liveness check.
    #[test]
    fn entry_live_at_matches_now_ge_expires_at_is_expired_convention() {
        let now = Instant::now();
        let future = now + crate::time::Duration::from_millis(10);
        let past = now - crate::time::Duration::from_millis(10);

        // `expires_at = None` never expires, regardless of `now`.
        assert!(TtlCache::<u32, u32>::entry_live_at(None, now));
        // `now < expires_at`: live.
        assert!(TtlCache::<u32, u32>::entry_live_at(Some(future), now));
        // `now == expires_at`: the boundary itself is NOT live.
        assert!(!TtlCache::<u32, u32>::entry_live_at(Some(now), now));
        // `now > expires_at`: not live.
        assert!(!TtlCache::<u32, u32>::entry_live_at(Some(past), now));
    }

    // PERF-1: `retain`'s eager sweep now samples the clock once per call (mirroring
    // `evict`) instead of once per entry. Expired entries must still be removed
    // unconditionally, regardless of what the predicate returns.
    #[test]
    fn retain_removes_expired_entries_regardless_of_predicate() {
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_millis(30))
            .build()
            .unwrap();
        c.cache_set(1, 10);
        std::thread::sleep(std::time::Duration::from_millis(80));
        // Inserted after the sleep: still live relative to the single `now` sampled
        // at the top of `retain`.
        c.cache_set(2, 20);

        // Predicate always says "keep" -- the expired entry must be swept anyway.
        c.retain(|_, _| true);

        assert_eq!(
            c.cache_size(),
            1,
            "expired entry must be removed even though the predicate kept it"
        );
        assert_eq!(
            c.cache_peek(&2),
            Some(&20),
            "live entry kept by the predicate must survive"
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
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_millis(30))
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
        assert_eq!(c.cache_peek(&2), Some(&20));
        assert_eq!(c.cache_peek(&3), None);
        assert_eq!(c.cache_peek(&4), Some(&40));
    }

    #[test]
    fn retain_with_panicking_on_evict_still_counts_eviction() {
        // The entry is removed from the map and counted BEFORE `on_evict` runs
        // (`HashMap::retain` returns `false`, dropping the entry, only after the
        // closure -- but the counter increment happens before the callback inside
        // that closure), so a panicking callback must not leave the eviction
        // uncounted.
        use std::panic::{AssertUnwindSafe, catch_unwind};
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_secs(60))
            .on_evict(|_k: &u32, _v: &u32| panic!("boom"))
            .build()
            .unwrap();
        c.cache_set(1, 10);
        let r = catch_unwind(AssertUnwindSafe(|| c.retain(|_, _| false)));
        assert!(r.is_err(), "on_evict should have panicked");
        assert_eq!(
            c.cache_evictions(),
            Some(1),
            "eviction must be counted even though on_evict panicked"
        );
    }

    #[test]
    fn cache_remove_entry_with_panicking_on_evict_still_counts_eviction() {
        // The entry is popped from the map and counted BEFORE `on_evict` runs, so a
        // panicking callback must not leave the removed entry uncounted.
        use std::panic::{AssertUnwindSafe, catch_unwind};
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_secs(60))
            .on_evict(|_k: &u32, _v: &u32| panic!("boom"))
            .build()
            .unwrap();
        c.cache_set(1, 10);
        let r = catch_unwind(AssertUnwindSafe(|| c.cache_remove_entry(&1u32)));
        assert!(r.is_err(), "on_evict should have panicked");
        assert_eq!(c.cache_peek(&1), None, "entry must still be removed");
        assert_eq!(
            c.cache_evictions(),
            Some(1),
            "eviction must be counted even though on_evict panicked"
        );
    }

    #[test]
    fn cache_get_lazy_sweep_with_panicking_on_evict_still_counts_eviction() {
        // `cache_get`'s lazy-sweep path removes the expired entry from the map and
        // counts the eviction BEFORE `on_evict` runs, so a panicking callback must
        // not leave the swept entry uncounted.
        use std::panic::{AssertUnwindSafe, catch_unwind};
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_millis(20))
            .on_evict(|_k: &u32, _v: &u32| panic!("boom"))
            .build()
            .unwrap();
        c.cache_set(1, 10);
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
        // value; `set_entry` counts the eviction BEFORE notifying, so a panicking
        // callback must not leave it uncounted.
        use std::panic::{AssertUnwindSafe, catch_unwind};
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_millis(20))
            .on_evict(|_k: &u32, _v: &u32| panic!("boom"))
            .build()
            .unwrap();
        c.cache_set(1, 10);
        std::thread::sleep(std::time::Duration::from_millis(80));
        let r = catch_unwind(AssertUnwindSafe(|| c.cache_set(1, 20)));
        assert!(r.is_err(), "on_evict should have panicked");
        assert_eq!(
            c.cache_evictions(),
            Some(1),
            "eviction must be counted even though on_evict panicked"
        );
    }

    // PERF-1: with `refresh_on_hit`, a hit must extend the entry's expiry by the
    // FULL configured ttl measured from the moment of the hit -- not by
    // ttl-minus-epsilon (e.g. from a stale/earlier clock read). Verified directly
    // against the stored `expires_at`, bracketed by clock reads taken immediately
    // before and after the hit.
    #[test]
    fn refresh_on_hit_extends_expiry_by_full_ttl_from_hit_time() {
        let ttl = crate::time::Duration::from_millis(200);
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(ttl)
            .refresh_on_hit(true)
            .build()
            .unwrap();
        c.cache_set(1, 100);
        std::thread::sleep(std::time::Duration::from_millis(80));

        let before = Instant::now();
        assert_eq!(c.cache_get(&1), Some(&100));
        let after = Instant::now();

        let expires_at = c
            .store
            .get(&1)
            .expect("entry must still be present after the hit")
            .expires_at
            .expect("ttl is configured, so the entry must carry an expiry");
        assert!(
            expires_at >= before + ttl,
            "refresh must extend by the FULL ttl measured from the hit, not less"
        );
        assert!(
            expires_at <= after + ttl,
            "refresh must not anchor to a clock read taken before the hit"
        );
    }

    // --- PERF-1 boundary coverage: every call site converted to `entry_live_at`
    // must preserve the exact `now >= expires_at` boundary, not just the pure
    // helper (already pinned above by
    // `entry_live_at_matches_now_ge_expires_at_is_expired_convention`).
    //
    // Each test below crafts a `TimedEntry` directly (the store field is
    // `pub(crate)`) with `expires_at` set to an `Instant` sampled just before the
    // call under test. Because the process clock is monotonic, the call's own
    // internal `Instant::now()` read is guaranteed to be `>= ` that sampled
    // instant, so this deterministically exercises the "tie or later" edge of the
    // boundary without needing a mock clock. A comfortably-future `expires_at`
    // exercises the live side, and `expires_at = None` exercises "never expires".

    #[test]
    fn cache_get_boundary_matches_now_ge_expires_at_convention() {
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_secs(60))
            .build()
            .unwrap();

        let tie = Instant::now();
        c.store.insert(
            1,
            TimedEntry {
                expires_at: Some(tie),
                value: 100,
            },
        );
        assert_eq!(
            c.cache_get(&1),
            None,
            "tie (now >= expires_at) must be a miss"
        );
        assert_eq!(c.cache_size(), 0, "expired entry must be swept on access");

        let future = Instant::now() + crate::time::Duration::from_millis(200);
        c.store.insert(
            2,
            TimedEntry {
                expires_at: Some(future),
                value: 200,
            },
        );
        assert_eq!(
            c.cache_get(&2),
            Some(&200),
            "now < expires_at must be a hit"
        );

        c.store.insert(
            3,
            TimedEntry {
                expires_at: None,
                value: 300,
            },
        );
        std::thread::sleep(std::time::Duration::from_millis(30));
        assert_eq!(
            c.cache_get(&3),
            Some(&300),
            "expires_at = None never expires"
        );
    }

    #[test]
    fn cache_get_mut_boundary_matches_now_ge_expires_at_convention() {
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_secs(60))
            .build()
            .unwrap();

        let tie = Instant::now();
        c.store.insert(
            1,
            TimedEntry {
                expires_at: Some(tie),
                value: 100,
            },
        );
        assert_eq!(
            c.cache_get_mut(&1),
            None,
            "tie (now >= expires_at) must be a miss"
        );
        assert_eq!(c.cache_size(), 0, "expired entry must be swept on access");

        let future = Instant::now() + crate::time::Duration::from_millis(200);
        c.store.insert(
            2,
            TimedEntry {
                expires_at: Some(future),
                value: 200,
            },
        );
        assert_eq!(
            c.cache_get_mut(&2).map(|v| *v),
            Some(200),
            "now < expires_at must be a hit"
        );

        c.store.insert(
            3,
            TimedEntry {
                expires_at: None,
                value: 300,
            },
        );
        std::thread::sleep(std::time::Duration::from_millis(30));
        assert_eq!(
            c.cache_get_mut(&3).map(|v| *v),
            Some(300),
            "expires_at = None never expires"
        );
    }

    #[test]
    fn cache_set_previous_value_liveness_boundary_matches_convention() {
        let fired = Arc::new(AtomicUsize::new(0));
        let fired2 = fired.clone();
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_secs(60))
            .on_evict(move |_k: &u32, _v: &u32| {
                fired2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();

        // Tie: `set_entry`'s previous-value liveness check must treat this as
        // expired -- filtered from the return, on_evict fires, eviction counted.
        let tie = Instant::now();
        c.store.insert(
            1,
            TimedEntry {
                expires_at: Some(tie),
                value: 100,
            },
        );
        assert_eq!(
            c.cache_set(1, 999),
            None,
            "tie (now >= expires_at) previous value must be filtered as expired"
        );
        assert_eq!(fired.load(Ordering::Relaxed), 1);
        assert_eq!(c.cache_evictions(), Some(1));

        // Comfortably future: previous value is still live and must be returned,
        // with no on_evict / eviction bump.
        let future = Instant::now() + crate::time::Duration::from_millis(200);
        c.store.insert(
            2,
            TimedEntry {
                expires_at: Some(future),
                value: 200,
            },
        );
        assert_eq!(
            c.cache_set(2, 888),
            Some(200),
            "now < expires_at: previous value must be returned, not filtered"
        );
        assert_eq!(
            fired.load(Ordering::Relaxed),
            1,
            "no new eviction for a live overwrite"
        );
        assert_eq!(c.cache_evictions(), Some(1));

        // None: previous value never expires, must always be returned.
        c.store.insert(
            3,
            TimedEntry {
                expires_at: None,
                value: 300,
            },
        );
        std::thread::sleep(std::time::Duration::from_millis(30));
        assert_eq!(
            c.cache_set(3, 777),
            Some(300),
            "expires_at = None previous value never expires"
        );
        assert_eq!(fired.load(Ordering::Relaxed), 1);
        assert_eq!(c.cache_evictions(), Some(1));
    }

    #[test]
    fn cache_get_or_set_with_mut_hit_liveness_boundary_matches_convention() {
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_secs(60))
            .build()
            .unwrap();

        // Tie: expired -> the factory MUST run (miss + expired-replace branch).
        let tie = Instant::now();
        c.store.insert(
            1,
            TimedEntry {
                expires_at: Some(tie),
                value: 100,
            },
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let calls2 = calls.clone();
        let val = c.cache_get_or_set_with_mut(1u32, move || {
            calls2.fetch_add(1, Ordering::Relaxed);
            999u32
        });
        assert_eq!(
            *val, 999,
            "tie must be treated as expired: factory value used"
        );
        assert_eq!(
            calls.load(Ordering::Relaxed),
            1,
            "factory must run at the tie boundary"
        );

        // Comfortably future: live -> factory must NOT run.
        let future = Instant::now() + crate::time::Duration::from_millis(200);
        c.store.insert(
            2,
            TimedEntry {
                expires_at: Some(future),
                value: 200,
            },
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let calls2 = calls.clone();
        let val = c.cache_get_or_set_with_mut(2u32, move || {
            calls2.fetch_add(1, Ordering::Relaxed);
            999u32
        });
        assert_eq!(
            *val, 200,
            "now < expires_at must be a hit: existing value returned"
        );
        assert_eq!(
            calls.load(Ordering::Relaxed),
            0,
            "factory must not run on a hit"
        );

        // None: never expires -> always a hit, even after elapsed time.
        c.store.insert(
            3,
            TimedEntry {
                expires_at: None,
                value: 300,
            },
        );
        std::thread::sleep(std::time::Duration::from_millis(30));
        let calls = Arc::new(AtomicUsize::new(0));
        let calls2 = calls.clone();
        let val = c.cache_get_or_set_with_mut(3u32, move || {
            calls2.fetch_add(1, Ordering::Relaxed);
            999u32
        });
        assert_eq!(*val, 300, "expires_at = None must always be a hit");
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn cache_try_get_or_set_with_mut_hit_liveness_boundary_matches_convention() {
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_secs(60))
            .build()
            .unwrap();

        let tie = Instant::now();
        c.store.insert(
            1,
            TimedEntry {
                expires_at: Some(tie),
                value: 100,
            },
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let calls2 = calls.clone();
        let val: Result<&mut u32, ()> = c.cache_try_get_or_set_with_mut(1u32, move || {
            calls2.fetch_add(1, Ordering::Relaxed);
            Ok(999u32)
        });
        assert_eq!(
            *val.unwrap(),
            999,
            "tie must be treated as expired: factory value used"
        );
        assert_eq!(
            calls.load(Ordering::Relaxed),
            1,
            "factory must run at the tie boundary"
        );

        let future = Instant::now() + crate::time::Duration::from_millis(200);
        c.store.insert(
            2,
            TimedEntry {
                expires_at: Some(future),
                value: 200,
            },
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let calls2 = calls.clone();
        let val: Result<&mut u32, ()> = c.cache_try_get_or_set_with_mut(2u32, move || {
            calls2.fetch_add(1, Ordering::Relaxed);
            Ok(999u32)
        });
        assert_eq!(
            *val.unwrap(),
            200,
            "now < expires_at must be a hit: existing value returned"
        );
        assert_eq!(
            calls.load(Ordering::Relaxed),
            0,
            "factory must not run on a hit"
        );

        c.store.insert(
            3,
            TimedEntry {
                expires_at: None,
                value: 300,
            },
        );
        std::thread::sleep(std::time::Duration::from_millis(30));
        let calls = Arc::new(AtomicUsize::new(0));
        let calls2 = calls.clone();
        let val: Result<&mut u32, ()> = c.cache_try_get_or_set_with_mut(3u32, move || {
            calls2.fetch_add(1, Ordering::Relaxed);
            Ok(999u32)
        });
        assert_eq!(*val.unwrap(), 300, "expires_at = None must always be a hit");
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn cache_get_with_expiry_status_boundary_matches_convention() {
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_secs(60))
            .build()
            .unwrap();

        let tie = Instant::now();
        c.store.insert(
            1,
            TimedEntry {
                expires_at: Some(tie),
                value: 100,
            },
        );
        assert_eq!(
            c.cache_get_with_expiry_status(&1u32),
            (Some(100), true),
            "tie (now >= expires_at) must report expired=true"
        );

        let future = Instant::now() + crate::time::Duration::from_millis(200);
        c.store.insert(
            2,
            TimedEntry {
                expires_at: Some(future),
                value: 200,
            },
        );
        assert_eq!(
            c.cache_get_with_expiry_status(&2u32),
            (Some(200), false),
            "now < expires_at must report expired=false"
        );

        c.store.insert(
            3,
            TimedEntry {
                expires_at: None,
                value: 300,
            },
        );
        std::thread::sleep(std::time::Duration::from_millis(30));
        assert_eq!(
            c.cache_get_with_expiry_status(&3u32),
            (Some(300), false),
            "expires_at = None must always report expired=false"
        );
    }

    #[test]
    fn retain_boundary_matches_now_ge_expires_at_convention() {
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_secs(60))
            .build()
            .unwrap();

        // Tie: expires_at == an instant sampled just before retain() is invoked.
        // retain's hoisted `now` is sampled strictly later, so this entry must be
        // removed unconditionally, even though the predicate says "keep".
        let tie = Instant::now();
        c.store.insert(
            1,
            TimedEntry {
                expires_at: Some(tie),
                value: 100,
            },
        );

        // None: never expires, so retain must only remove it if the predicate says so.
        c.store.insert(
            2,
            TimedEntry {
                expires_at: None,
                value: 200,
            },
        );

        c.retain(|_, _| true);

        assert_eq!(
            c.cache_size(),
            1,
            "tie entry must be swept regardless of predicate"
        );
        assert_eq!(
            c.cache_peek(&2),
            Some(&200),
            "never-expiring entry kept by predicate must survive"
        );
    }

    // PERF-1: `retain`'s hoisted `now` is a single snapshot taken BEFORE the sweep
    // begins (mirroring `evict`). An entry that was live at that snapshot must
    // stay judged live for the whole pass, even if real wall-clock time advances
    // past its expiry while the predicate is busy on other entries. A regression
    // back to a per-entry `Instant::now()` read would evict entries examined later
    // in the pass despite every entry having been live when `retain()` began.
    #[test]
    fn retain_judges_every_entry_against_the_pass_start_snapshot() {
        let margin = crate::time::Duration::from_millis(35);
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_secs(60))
            .build()
            .unwrap();

        // 6 entries, all live at "now" (the snapshot retain() will take), all
        // sharing the same expires_at margin above it.
        let base = Instant::now();
        let expires_at = base + margin;
        for k in 0..6u32 {
            c.store.insert(
                k,
                TimedEntry {
                    expires_at: Some(expires_at),
                    value: k,
                },
            );
        }

        // The predicate sleeps on every call. With 6 entries at 20ms each, the
        // cumulative elapsed time crosses the 35ms margin partway through the
        // pass -- enough that a per-entry `Instant::now()` read would see entries
        // examined later in the pass as expired, even though every entry was live
        // when `retain()` began.
        c.retain(|_, _| {
            std::thread::sleep(std::time::Duration::from_millis(20));
            true
        });

        assert_eq!(
            c.cache_size(),
            6,
            "every entry must be judged live against the pass-start snapshot, \
             regardless of how long the predicate takes on other entries"
        );
    }

    // Confirms `CachedIter::iter` was deliberately NOT changed to hoist `now`
    // (unlike `retain`/`evict`): a lazy iterator advanced after a delay must judge
    // each item against a clock read taken at production time, not at the time
    // `iter()` was called.
    #[test]
    fn iter_judges_each_item_at_production_time_not_at_iter_call_time() {
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_secs(60))
            .build()
            .unwrap();
        let expires_at = Instant::now() + crate::time::Duration::from_millis(30);
        c.store.insert(
            1,
            TimedEntry {
                expires_at: Some(expires_at),
                value: 100,
            },
        );

        // The entry is live at the moment `iter()` is called -- building the lazy
        // iterator does not itself read the clock.
        let mut it = c.iter();

        // Advance real time past the entry's expiry BEFORE consuming the iterator.
        std::thread::sleep(std::time::Duration::from_millis(60));

        assert_eq!(
            it.next(),
            None,
            "item must be judged expired at consumption time, proving `iter` samples \
             the clock per item rather than hoisting a single snapshot from iter() call time"
        );
    }

    // PERF-1 / CORE-3 (sync): the expiry for a freshly-replaced expired entry must
    // be anchored to the clock read taken AFTER the factory resolves, not the
    // sample taken before the liveness check. A factory slower than the ttl proves
    // this: if the expiry were computed from the pre-factory sample, the
    // freshly-inserted entry would already be expired the instant it lands.
    #[test]
    fn cache_get_or_set_with_mut_expiry_anchored_after_slow_factory() {
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_millis(40))
            .build()
            .unwrap();
        c.cache_set(1, 100);
        std::thread::sleep(std::time::Duration::from_millis(60)); // now expired

        let val = c.cache_get_or_set_with_mut(1u32, || {
            std::thread::sleep(std::time::Duration::from_millis(120)); // 3x the ttl
            200u32
        });
        assert_eq!(*val, 200);
        assert_eq!(
            c.cache_peek(&1),
            Some(&200),
            "entry must be live immediately after a factory slower than the ttl \
             resolves -- expiry must be anchored post-factory, not pre-factory"
        );
    }

    #[test]
    fn cache_try_get_or_set_with_mut_expiry_anchored_after_slow_factory() {
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_millis(40))
            .build()
            .unwrap();
        c.cache_set(1, 100);
        std::thread::sleep(std::time::Duration::from_millis(60)); // now expired

        let val: Result<&mut u32, ()> = c.cache_try_get_or_set_with_mut(1u32, || {
            std::thread::sleep(std::time::Duration::from_millis(120)); // 3x the ttl
            Ok(200u32)
        });
        assert_eq!(*val.unwrap(), 200);
        assert_eq!(
            c.cache_peek(&1),
            Some(&200),
            "entry must be live immediately after a factory slower than the ttl \
             resolves -- expiry must be anchored post-factory, not pre-factory"
        );
    }

    // Tight bracketing refresh-on-hit tests, mirroring
    // `refresh_on_hit_extends_expiry_by_full_ttl_from_hit_time` (for `cache_get`)
    // on the other call sites the `now`-threading change touched. Threading made
    // the liveness-check sample and the refresh-compute sample the SAME value at
    // each site, so these can no longer distinguish "pre-check" from "post-check"
    // anchoring (that distinction no longer exists structurally) -- they instead
    // pin the property that actually matters: the persisted `expires_at` reflects
    // a full ttl measured from within the call, not a partial/stale extension.

    #[test]
    fn cache_get_mut_refresh_extends_expiry_by_full_ttl_from_hit_time() {
        let ttl = crate::time::Duration::from_millis(200);
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(ttl)
            .refresh_on_hit(true)
            .build()
            .unwrap();
        c.cache_set(1, 100);
        std::thread::sleep(std::time::Duration::from_millis(80));

        let before = Instant::now();
        assert_eq!(c.cache_get_mut(&1).map(|v| *v), Some(100));
        let after = Instant::now();

        let expires_at = c
            .store
            .get(&1)
            .expect("entry must still be present after the hit")
            .expires_at
            .expect("ttl is configured, so the entry must carry an expiry");
        assert!(
            expires_at >= before + ttl,
            "refresh must extend by the FULL ttl measured from the hit, not less"
        );
        assert!(
            expires_at <= after + ttl,
            "refresh must not anchor to a clock read taken before the hit"
        );
    }

    #[test]
    fn cache_get_or_set_with_mut_refresh_extends_expiry_by_full_ttl_from_hit_time() {
        let ttl = crate::time::Duration::from_millis(200);
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(ttl)
            .refresh_on_hit(true)
            .build()
            .unwrap();
        c.cache_set(1, 100);
        std::thread::sleep(std::time::Duration::from_millis(80));

        let before = Instant::now();
        let val = c.cache_get_or_set_with_mut(1u32, || 999u32);
        assert_eq!(*val, 100, "still a hit, factory ignored");
        let after = Instant::now();

        let expires_at = c
            .store
            .get(&1)
            .expect("entry must still be present after the hit")
            .expires_at
            .expect("ttl is configured, so the entry must carry an expiry");
        assert!(
            expires_at >= before + ttl,
            "refresh must extend by the FULL ttl measured from the hit, not less"
        );
        assert!(
            expires_at <= after + ttl,
            "refresh must not anchor to a clock read taken before the hit"
        );
    }

    #[test]
    fn cache_try_get_or_set_with_mut_refresh_extends_expiry_by_full_ttl_from_hit_time() {
        let ttl = crate::time::Duration::from_millis(200);
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(ttl)
            .refresh_on_hit(true)
            .build()
            .unwrap();
        c.cache_set(1, 100);
        std::thread::sleep(std::time::Duration::from_millis(80));

        let before = Instant::now();
        let val: Result<&mut u32, ()> = c.cache_try_get_or_set_with_mut(1u32, || Ok(999u32));
        assert_eq!(*val.unwrap(), 100, "still a hit, factory ignored");
        let after = Instant::now();

        let expires_at = c
            .store
            .get(&1)
            .expect("entry must still be present after the hit")
            .expires_at
            .expect("ttl is configured, so the entry must carry an expiry");
        assert!(
            expires_at >= before + ttl,
            "refresh must extend by the FULL ttl measured from the hit, not less"
        );
        assert!(
            expires_at <= after + ttl,
            "refresh must not anchor to a clock read taken before the hit"
        );
    }

    #[test]
    fn cache_get_with_expiry_status_refresh_extends_expiry_by_full_ttl_from_hit_time() {
        let ttl = crate::time::Duration::from_millis(200);
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(ttl)
            .refresh_on_hit(true)
            .build()
            .unwrap();
        c.cache_set(1, 100);
        std::thread::sleep(std::time::Duration::from_millis(80));

        let before = Instant::now();
        assert_eq!(c.cache_get_with_expiry_status(&1u32), (Some(100), false));
        let after = Instant::now();

        let expires_at = c
            .store
            .get(&1)
            .expect("entry must still be present after the hit")
            .expires_at
            .expect("ttl is configured, so the entry must carry an expiry");
        assert!(
            expires_at >= before + ttl,
            "refresh must extend by the FULL ttl measured from the hit, not less"
        );
        assert!(
            expires_at <= after + ttl,
            "refresh must not anchor to a clock read taken before the hit"
        );
    }

    #[test]
    fn peek_expires_at_absent_key_returns_none_none() {
        let c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_secs(60))
            .build()
            .unwrap();
        assert_eq!(c.cache_peek_expires_at(&1u32), (None, None));
        assert_eq!(c.peek_expires_at(&1u32), (None, None));
    }

    #[test]
    fn peek_expires_at_live_entry_returns_the_stored_future_deadline() {
        let ttl = crate::time::Duration::from_secs(60);
        let mut c: TtlCache<u32, u32> = TtlCache::builder().ttl(ttl).build().unwrap();
        let before = Instant::now();
        c.cache_set(1, 100);
        let after = Instant::now();

        let stored = c
            .store
            .get(&1)
            .expect("entry must be present")
            .expires_at
            .expect("a configured ttl must record a deadline");

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
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_secs(60))
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
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_millis(20))
            .build()
            .unwrap();
        c.cache_set(1, 100);
        std::thread::sleep(std::time::Duration::from_millis(60));

        let (value, expires_at) = c.cache_peek_expires_at(&1u32);
        assert_eq!(value, Some(100), "an expired entry is still returned");
        let expires_at = expires_at.expect("an expired entry still carries its deadline");
        assert!(expires_at <= Instant::now(), "the deadline is in the past");
        // Not removed by the peek: a second peek sees the same entry and deadline.
        assert_eq!(c.cache_size(), 1);
        assert_eq!(
            c.cache_peek_expires_at(&1u32),
            (Some(100), Some(expires_at))
        );
    }

    #[test]
    fn peek_expires_at_deadline_is_past_exactly_when_peek_reports_expired() {
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_millis(20))
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
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_secs(60))
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
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_millis(200))
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

    // Gap 1: an overflowing TTL (compute_expires_at's now.checked_add(ttl) -> None) must be
    // reported by peek_expires_at identically to expiry being disabled: (Some(v), None). The
    // implementor's own regression test (cache_set_with_ttl_overflow_stores_never_expiring_entry)
    // never called peek_expires_at, so this pins the actual public-API observation.
    #[test]
    fn peek_expires_at_overflowing_ttl_reports_no_deadline() {
        use crate::CacheTtl;
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_secs(60))
            .build()
            .unwrap();
        c.set_ttl(crate::time::Duration::MAX);
        c.cache_set(1, 42);
        assert_eq!(
            c.cache_peek_expires_at(&1u32),
            (Some(42), None),
            "an overflowing ttl must be indistinguishable, via peek_expires_at, from expiry disabled"
        );
        assert_eq!(c.peek_expires_at(&1u32), c.cache_peek_expires_at(&1u32));
    }

    // Gap 2: changing the store's ttl (including disabling it) must NOT retroactively touch a
    // deadline an already-stored entry carries -- only fresh inserts/refreshes are affected by
    // the new ttl. peek_expires_at must keep reporting the stale deadline.
    #[test]
    fn peek_expires_at_reports_stale_deadline_after_ttl_change() {
        use crate::CacheTtl;
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_secs(60))
            .build()
            .unwrap();
        c.cache_set(1, 100);
        let (_, original) = c.cache_peek_expires_at(&1u32);
        let original = original.expect("a configured ttl must record a deadline");

        // Shrinking the ttl must not touch the entry already stored.
        c.set_ttl(crate::time::Duration::from_secs(5));
        assert_eq!(
            c.cache_peek_expires_at(&1u32),
            (Some(100), Some(original)),
            "changing the store ttl must not retroactively rewrite an existing entry's deadline"
        );

        // Disabling the ttl entirely (unset_ttl, i.e. set_ttl(ZERO)) must not clear the
        // deadline either -- only future inserts/refreshes are affected.
        c.unset_ttl();
        assert_eq!(
            c.cache_peek_expires_at(&1u32),
            (Some(100), Some(original)),
            "disabling the ttl must not clear an already-stored entry's deadline"
        );

        // A fresh insert after disabling the ttl, in contrast, has no deadline.
        c.cache_set(2, 200);
        assert_eq!(c.cache_peek_expires_at(&2u32), (Some(200), None));
    }

    // Gap: the crate's documented convention is `now >= expires_at` means expired (see the
    // boundary test at src/stores/lru_ttl.rs:2775). Pin that peek_expires_at's raw deadline and
    // cache_peek_with_expiry_status's liveness judgement agree exactly at the tie, deterministically
    // (no sleep): `tie` is sampled before it is written into the entry, so by the time it is read
    // back, real "now" is guaranteed to be >= `tie`.
    #[test]
    fn peek_expires_at_boundary_matches_now_ge_expires_at_convention() {
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_secs(3600))
            .build()
            .unwrap();
        c.cache_set(1, 100);
        let tie = Instant::now();
        c.store.get_mut(&1).unwrap().expires_at = Some(tie);

        assert_eq!(c.cache_peek_expires_at(&1u32), (Some(100), Some(tie)));
        assert_eq!(
            c.cache_peek_with_expiry_status(&1u32),
            (Some(100), true),
            "now == expires_at must be treated as expired, matching the now >= expires_at convention"
        );
    }

    // Gap: peek_expires_at must reflect physical removal -- both the lazy sweep folded into
    // evict() and an explicit cache_remove -- by reporting the absent-key shape afterward.
    #[test]
    fn peek_expires_at_reports_absent_after_evict_removes_the_entry() {
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_millis(20))
            .build()
            .unwrap();
        c.cache_set(1, 100);
        std::thread::sleep(std::time::Duration::from_millis(60));

        // Expired but not yet swept: peek still reports it with its past deadline.
        let (value, expires_at) = c.cache_peek_expires_at(&1u32);
        assert_eq!(value, Some(100));
        assert!(expires_at.unwrap() <= Instant::now());

        assert_eq!(
            c.evict(),
            1,
            "evict must physically remove the expired entry"
        );
        assert_eq!(
            c.cache_peek_expires_at(&1u32),
            (None, None),
            "a physically removed entry must be reported as absent"
        );
    }

    #[test]
    fn peek_expires_at_reports_absent_after_cache_remove() {
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_secs(60))
            .build()
            .unwrap();
        c.cache_set(1, 100);
        assert_eq!(c.cache_remove(&1u32), Some(100));
        assert_eq!(c.cache_peek_expires_at(&1u32), (None, None));
        assert_eq!(c.peek_expires_at(&1u32), (None, None));
    }

    // Gap 5: the ergonomic alias must agree with the canonical method across every return
    // shape the contract defines, not just the absent-key case the implementor already covered.
    #[test]
    fn peek_expires_at_alias_matches_canonical_across_all_return_shapes() {
        use crate::CacheTtl;
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_millis(20))
            .build()
            .unwrap();

        // absent
        assert_eq!(c.peek_expires_at(&1u32), c.cache_peek_expires_at(&1u32));
        assert_eq!(c.peek_expires_at(&1u32), (None, None));

        // live
        c.cache_set(1, 100);
        assert_eq!(c.peek_expires_at(&1u32), c.cache_peek_expires_at(&1u32));
        assert!(c.peek_expires_at(&1u32).1.unwrap() > Instant::now());

        // expired, not removed
        std::thread::sleep(std::time::Duration::from_millis(60));
        assert_eq!(c.peek_expires_at(&1u32), c.cache_peek_expires_at(&1u32));
        assert!(c.peek_expires_at(&1u32).1.unwrap() <= Instant::now());

        // never-expiring
        c.unset_ttl();
        c.cache_set(2, 200);
        assert_eq!(c.peek_expires_at(&2u32), (Some(200), None));
        assert_eq!(c.peek_expires_at(&2u32), c.cache_peek_expires_at(&2u32));
    }

    // Gap 4: nothing else in the suite calls CacheExpiry through a generic `T: CacheExpiry<K, V>`
    // bound or through `cached::prelude::*` -- both a monomorphization/dyn-compat regression and
    // a prelude export regression would go uncaught otherwise.
    #[test]
    fn cache_expiry_is_reachable_through_a_generic_bound_and_the_prelude() {
        // Mirrors an external `use cached::prelude::*;`, independent of the direct
        // `use crate::CacheExpiry` import at the top of this file.
        use crate::prelude::*;

        fn peek_via_bound<T: CacheExpiry<u32, u32>>(
            store: &T,
            key: &u32,
        ) -> (Option<u32>, Option<crate::time::Instant>) {
            store.cache_peek_expires_at(key)
        }

        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_secs(60))
            .build()
            .unwrap();
        c.cache_set(1, 100);

        let (value, expires_at) = peek_via_bound(&c, &1);
        assert_eq!(value, Some(100));
        assert!(expires_at.is_some());
        assert_eq!(
            peek_via_bound(&c, &2),
            (None, None),
            "absent key via the generic bound"
        );
    }

    // --- CacheExpiry::cache_expires_at (the value-free read) ---

    #[test]
    fn expires_at_absent_key_returns_false_none() {
        let c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_secs(60))
            .build()
            .unwrap();
        assert_eq!(c.cache_expires_at(&1u32), (false, None));
        assert_eq!(c.expires_at(&1u32), (false, None));
    }

    #[test]
    fn expires_at_live_entry_returns_the_stored_future_deadline() {
        let ttl = crate::time::Duration::from_secs(60);
        let mut c: TtlCache<u32, u32> = TtlCache::builder().ttl(ttl).build().unwrap();
        let before = Instant::now();
        c.cache_set(1, 100);
        let after = Instant::now();

        let stored = c
            .store
            .get(&1)
            .expect("entry must be present")
            .expires_at
            .expect("a configured ttl must record a deadline");

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
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_secs(60))
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
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_millis(20))
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

    // The two reads must never disagree: same deadline, and the presence flag must track whether
    // the value-bearing read returned `Some`.
    #[test]
    fn expires_at_agrees_with_peek_expires_at_across_all_return_shapes() {
        use crate::CacheTtl;
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_millis(20))
            .build()
            .unwrap();

        let check = |c: &TtlCache<u32, u32>, k: u32, label: &str| {
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
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_secs(60))
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
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_millis(200))
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

    // The point of moving `V: Clone` off the impl block and onto the value-bearing methods: a
    // deadline read must work on a cache whose value type is not `Clone` at all. The generic
    // helper carries no `V: Clone` bound anywhere, so this fails to compile if the bound creeps
    // back onto either the trait method or the impl.
    #[test]
    fn expires_at_reads_a_deadline_for_a_value_type_that_is_not_clone() {
        #[derive(Debug, PartialEq)]
        struct NotClone(u32);

        fn deadline<K: Hash + Eq, V>(c: &TtlCache<K, V>, k: &K) -> (bool, Option<Instant>) {
            c.cache_expires_at(k)
        }

        let ttl = crate::time::Duration::from_secs(60);
        let mut c: TtlCache<u32, NotClone> = TtlCache::builder().ttl(ttl).build().unwrap();
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

    // The value-free read must be reachable through a generic `T: CacheExpiry<K, V>` bound and
    // through the prelude, exactly like its value-bearing sibling.
    #[test]
    fn cache_expires_at_is_reachable_through_a_generic_bound_and_the_prelude() {
        use crate::prelude::*;

        fn deadline_via_bound<T: CacheExpiry<u32, u32>>(
            store: &T,
            key: &u32,
        ) -> (bool, Option<crate::time::Instant>) {
            store.cache_expires_at(key)
        }

        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_secs(60))
            .build()
            .unwrap();
        c.cache_set(1, 100);

        let (present, expires_at) = deadline_via_bound(&c, &1);
        assert!(present);
        assert!(expires_at.is_some());
        assert_eq!(
            deadline_via_bound(&c, &2),
            (false, None),
            "absent key via the generic bound"
        );
    }

    // The `now >= t` tie convention: the deadline the value-free read reports must be judged
    // expired at exactly the instant `cache_peek_with_expiry_status` calls the entry expired.
    #[test]
    fn expires_at_boundary_matches_now_ge_expires_at_convention() {
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_secs(3600))
            .build()
            .unwrap();
        c.cache_set(1, 100);
        let tie = Instant::now();
        c.store.get_mut(&1).unwrap().expires_at = Some(tie);

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
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_millis(20))
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

    // A generic bound, so this can only reach the trait method: the inherent
    // `cache_clear_with_on_evict` wins at a concrete call site.
    fn clear_with_on_evict_through_trait<T: crate::CacheClearWithOnEvict>(cache: &mut T) {
        cache.cache_clear_with_on_evict();
    }

    #[test]
    fn cache_clear_with_on_evict_through_trait_fires_for_all_entries() {
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

        clear_with_on_evict_through_trait(&mut c);
        assert_eq!(c.cache_size(), 0);
        assert_eq!(count.load(Ordering::Relaxed), 3);
        assert_eq!(c.cache_evictions(), Some(3));
        // The plain clear stays silent, so the trait method is not just an alias for it.
        c.cache_set(4, 40);
        c.cache_clear();
        assert_eq!(count.load(Ordering::Relaxed), 3);
        assert_eq!(c.cache_evictions(), Some(3));
    }
}
