use std::hash::Hash;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[cfg(feature = "ahash")]
use ahash::RandomState;
#[cfg(not(feature = "ahash"))]
use std::collections::hash_map::RandomState;

use std::collections::HashMap;

use crate::time::{Duration, Instant};
use crate::{
    CacheMetrics, ConcurrentCacheBase, ConcurrentCacheEvict, ConcurrentCacheExpiry,
    ConcurrentCachePeek, ConcurrentCacheRefreshOnHit, ConcurrentCacheTtl, ConcurrentCached,
    ConcurrentCloneCached,
};
#[cfg(feature = "async_core")]
use crate::{ConcurrentCachePeekAsync, ConcurrentCachedAsync};
#[cfg(feature = "async_core")]
use core::future::Future;

use super::{
    CachePadded, DefaultShardHasher, Shard, ShardHasher, checked_shard_count, decode_ttl,
    encode_ttl, shard_index,
};
use crate::stores::{BuildError, TimedEntry};

type OnEvict<K, V> = Arc<dyn Fn(&K, &V) + Send + Sync>;

#[allow(clippy::type_complexity)]
struct TtlInner<K, V, H> {
    shards: Box<[CachePadded<Shard<HashMap<K, TimedEntry<V>, RandomState>>>]>,
    shard_mask: usize,
    hasher: H,
    on_evict: Option<OnEvict<K, V>>,
    /// TTL in nanoseconds, or `0` to mean expiry is disabled (entries never expire).
    /// A zero stored value is the single sentinel for "no expiry"; there is no separate
    /// `ttl_set` flag. `unset_ttl`/`set_ttl(0)` store `0`; `set_ttl(nonzero)` stores the ttl.
    ttl_nanos: AtomicU64,
    refresh: AtomicBool,
}

/// Judge a stored entry's expiry against an already-sampled instant.
///
/// `expires_at = None` means never-expires (TTL was disabled at insert time); otherwise the
/// entry is expired when `now >= expires_at`. Callers sample the clock **once** per operation
/// and thread that instant through, so every entry a single call inspects is judged against
/// the same instant (the discipline `evict`/`retain` already followed).
#[inline]
fn expired_at<V>(entry: &TimedEntry<V>, now: Instant) -> bool {
    entry.expires_at.is_some_and(|t| now >= t)
}

/// A fully-concurrent, partitioned, TTL-bounded in-memory cache.
///
/// Wraps an `Arc` — `clone()` is an Arc-share (shared state), not a deep copy.
/// Use [`deep_clone`](ShardedTtlCache::deep_clone) to get an independent copy.
///
/// **Note**: reads return owned values cloned from under the shard lock, so `V` must
/// implement `Clone`.
///
/// Read hits use a **shared read lock** per shard by default. When `refresh_on_hit` is enabled,
/// read hits acquire an exclusive **write lock** to update the entry's TTL timestamp — the same
/// trade-off as LRU variants. Disable `refresh_on_hit` if read-lock scalability is a priority.
///
/// **`len` / `evict` contract**: `len()` (the inherent method) returns the raw stored entry
/// count across all shards and may include expired-but-not-yet-swept entries. Call `evict()`
/// (via [`ConcurrentCacheEvict`](crate::ConcurrentCacheEvict)) to physically remove expired
/// entries and obtain an accurate live count. Sharded stores do not implement `CachedIter`.
///
/// The runtime TTL controls (`ttl` / `set_ttl` / `try_set_ttl` / `unset_ttl`) live on
/// [`ConcurrentCacheTtl`](crate::ConcurrentCacheTtl), and the refresh-on-hit controls
/// (`refresh_on_hit` / `set_refresh_on_hit`) on
/// [`ConcurrentCacheRefreshOnHit`](crate::ConcurrentCacheRefreshOnHit); import them (or
/// `cached::prelude::*`) to call them. Builder setters are unaffected.
///
/// The shard-selection hasher `H` defaults to [`DefaultShardHasher`] (ahash-backed when the
/// `ahash` feature is enabled, otherwise `std::collections::hash_map::RandomState`), so
/// `ShardedTtlCache<K, V>` names the common case. To use a custom [`ShardHasher`], call
/// [`ShardedTtlCache::builder()`] and then [`hasher`](ShardedTtlCacheBuilder::hasher), which
/// switches `H` to your hasher.
///
/// **Note**: this type's inherent methods (`get`, `set`, `remove`, `remove_entry`, `delete`,
/// `contains`, `peek`) return unwrapped values (`Option<V>`, `bool`, ...) and take call-site
/// priority over the same-named [`ConcurrentCached`] trait methods, which return
/// `Result<_, Self::Error>` instead. A `.unwrap()` chained onto one of these inherent calls is
/// therefore `Option::unwrap`, not `Result::unwrap`: `cache.set(k, v).unwrap()` panics on a
/// **fresh insert**, because there is no previous value to unwrap, not because the operation
/// failed. To reach the fallible trait form instead (which never panics on a fresh insert), name
/// it explicitly through the trait, e.g. `ConcurrentCached::cache_set(&cache, k, v)` or
/// `ConcurrentCachedExt::set(&cache, k, v)`.
pub struct ShardedTtlCache<K, V, H = DefaultShardHasher> {
    inner: Arc<TtlInner<K, V, H>>,
}

impl<K, V, H> Clone for ShardedTtlCache<K, V, H> {
    /// Arc-share clone — both handles point to the same underlying cache.
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<K, V, H> std::fmt::Debug for ShardedTtlCache<K, V, H> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ttl = self.ttl_duration_impl();
        f.debug_struct("ShardedTtlCache")
            .field("shards", &self.inner.shards.len())
            .field("ttl", &ttl)
            .finish_non_exhaustive()
    }
}

impl<K, V, H> ShardedTtlCache<K, V, H> {
    /// Resolve the currently configured TTL, independent of hasher bounds.
    ///
    /// Returns `None` when expiry is disabled (entries never expire), otherwise
    /// `Some(ttl)`.
    #[inline]
    fn ttl_duration_impl(&self) -> Option<Duration> {
        decode_ttl(self.inner.ttl_nanos.load(Ordering::Relaxed))
    }
}

impl<K, V> ShardedTtlCache<K, V, DefaultShardHasher>
where
    K: Hash + Eq,
{
    /// Construct a ready-to-use [`ShardedTtlCache`] with the given `ttl`, the
    /// [`DefaultShardHasher`], and a default shard count.
    ///
    /// For a custom hasher, shard count, `refresh_on_hit`, or `on_evict`, use
    /// [`builder`](Self::builder).
    ///
    /// # Panics
    ///
    /// Panics if `ttl` is zero. Use [`builder`](Self::builder) with
    /// [`build`](ShardedTtlCacheBuilder::build) to handle a zero TTL without panicking.
    #[must_use]
    pub fn new(ttl: Duration) -> ShardedTtlCache<K, V> {
        Self::builder()
            .ttl(ttl)
            .build()
            .expect("ShardedTtlCache::new requires a non-zero ttl")
    }

    /// Return a builder for constructing a [`ShardedTtlCache`].
    ///
    /// The builder starts with the [`DefaultShardHasher`]. To use a custom hasher, call
    /// [`hasher`](ShardedTtlCacheBuilder::hasher) on the returned builder; it switches the
    /// builder's hasher type and `build` then yields a `ShardedTtlCache<K, V, H>` over that
    /// hasher. `new` and `builder` exist only on the default-hasher instantiation
    /// `ShardedTtlCache<K, V, DefaultShardHasher>`, so a custom hasher is always introduced
    /// via `hasher`, never a `ShardedTtlCache::<_, _, H>` turbofish.
    #[must_use]
    pub fn builder() -> ShardedTtlCacheBuilder<K, V, DefaultShardHasher> {
        ShardedTtlCacheBuilder::default()
    }
}

impl<K, V, H> ShardedTtlCache<K, V, H>
where
    K: Hash + Eq,
    H: ShardHasher<K>,
{
    #[inline]
    fn shard_of(&self, k: &K) -> &CachePadded<Shard<HashMap<K, TimedEntry<V>, RandomState>>> {
        let h = self.inner.hasher.shard_hash(k);
        &self.inner.shards[shard_index(h, self.inner.shard_mask)]
    }

    #[inline]
    fn ttl_duration(&self) -> Option<Duration> {
        self.ttl_duration_impl()
    }

    #[inline]
    fn is_expired(&self, entry: &TimedEntry<V>) -> bool {
        expired_at(entry, Instant::now())
    }

    /// Compute the expiry instant for a new or refreshed entry given the current TTL.
    /// TTL is clamped to u64::MAX nanos (~584 years), so `checked_add` overflow is
    /// practically unreachable; if it does overflow, the entry becomes never-expires (`None`).
    fn compute_expires_at(&self, now: Instant) -> Option<Instant> {
        let nanos = self.inner.ttl_nanos.load(Ordering::Relaxed);
        if nanos == 0 {
            None
        } else {
            let ttl = Duration::from_nanos(nanos);
            now.checked_add(ttl)
        }
    }
}

impl<K: Clone + Hash + Eq, V: Clone, H: ShardHasher<K>> ShardedTtlCache<K, V, H> {
    /// Return an independent deep copy of this cache — entries and metrics are
    /// duplicated, not shared. In most cases [`Clone::clone`] (Arc-share) is
    /// what you want.
    #[must_use]
    pub fn deep_clone(&self) -> Self {
        let n = self.inner.shards.len();
        let shards = (0..n)
            .map(|i| {
                // Load the hit/miss counters under the read lock so the metrics snapshot is
                // consistent with the entry snapshot (B4: loading after drop(guard) could yield
                // counters newer than the cloned entries).
                let guard = self.inner.shards[i].lock.read();
                let store_copy = guard.clone();
                let hits = self.inner.shards[i].hits.load(Ordering::Relaxed);
                let misses = self.inner.shards[i].misses.load(Ordering::Relaxed);
                let evictions = self.inner.shards[i].evictions.load(Ordering::Relaxed);
                drop(guard);
                let shard = Shard {
                    lock: parking_lot::RwLock::new(store_copy),
                    hits: AtomicU64::new(hits),
                    misses: AtomicU64::new(misses),
                    evictions: AtomicU64::new(evictions),
                };
                CachePadded(shard)
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            inner: Arc::new(TtlInner {
                shards,
                shard_mask: self.inner.shard_mask,
                hasher: self.inner.hasher.clone(),
                on_evict: self.inner.on_evict.clone(),
                ttl_nanos: AtomicU64::new(self.inner.ttl_nanos.load(Ordering::Relaxed)),
                refresh: AtomicBool::new(self.inner.refresh.load(Ordering::Relaxed)),
            }),
        }
    }
}

impl<K, V, H: ShardHasher<K>> ShardedTtlCache<K, V, H>
where
    K: Hash + Eq,
    V: Clone,
{
    /// Retrieve a cached value, returning `None` on a miss or if the entry has expired.
    ///
    /// This is the infallible ergonomic API for the concrete type. Generic code over
    /// [`ConcurrentCached`] should use the `Result`-returning trait methods (`cache_get` or the
    /// `get` alias from [`ConcurrentCachedExt`](crate::ConcurrentCachedExt)), callable as
    /// `ConcurrentCachedExt::get(&store, k)` when this inherent method is in scope.
    #[must_use]
    pub fn get(&self, k: &K) -> Option<V> {
        ConcurrentCached::cache_get(self, k).unwrap()
    }

    /// Insert a key-value pair and return the previous value, if any.
    ///
    /// This is the infallible ergonomic API for the concrete type. Unlike the trait's
    /// [`cache_set`](ConcurrentCached::cache_set) (which returns `Result<Option<V>, _>`), this
    /// inherent form returns the displaced `Option<V>` directly, so `.set(k, v).unwrap()` panics
    /// on a fresh insert -- there is no prior value to unwrap.
    pub fn set(&self, k: K, v: V) -> Option<V> {
        ConcurrentCached::cache_set(self, k, v).unwrap()
    }

    /// Return the cached value for `k`, or compute `f()`, store it, and return it.
    ///
    /// Infallible ergonomic API for the concrete type. As an inherent method it takes
    /// resolution priority over
    /// [`ConcurrentCachedExt::get_or_set_with`](crate::ConcurrentCachedExt::get_or_set_with)
    /// (which returns `Result<V, Infallible>`), so no `.unwrap()` is needed at the call site.
    ///
    /// Non-atomic get-then-set: on a miss another thread may store a value for the same key
    /// between the get and the set. See
    /// [`ConcurrentCached::cache_get_or_set_with`](crate::ConcurrentCached::cache_get_or_set_with).
    pub fn get_or_set_with<F: FnOnce() -> V>(&self, k: K, f: F) -> V {
        ConcurrentCached::cache_get_or_set_with(self, k, f).unwrap()
    }

    /// Remove a cached value and return it if the entry was live.
    ///
    /// This is the infallible ergonomic API for the concrete type.
    pub fn remove(&self, k: &K) -> Option<V> {
        ConcurrentCached::cache_remove(self, k).unwrap()
    }

    /// Remove a cached entry and return the stored key and value, if present.
    ///
    /// This is the infallible ergonomic API for the concrete type.
    pub fn remove_entry(&self, k: &K) -> Option<(K, V)> {
        ConcurrentCached::cache_remove_entry(self, k).unwrap()
    }

    /// Delete a cached entry without returning the value. Returns `true` if an entry was removed.
    ///
    /// This is the infallible ergonomic API for the concrete type.
    pub fn delete(&self, k: &K) -> bool {
        ConcurrentCached::cache_delete(self, k).unwrap()
    }

    /// Remove all entries from every shard and reset metrics.
    ///
    /// This is the infallible ergonomic API for the concrete type.
    pub fn reset(&self) {
        ConcurrentCached::cache_reset(self).unwrap()
    }

    /// Return true if a live (not expired) value is stored for `k`. Peek-based: no recency update, no hit/miss metrics.
    #[must_use]
    pub fn contains(&self, k: &K) -> bool {
        ConcurrentCached::cache_contains(self, k).unwrap()
    }

    /// Return a clone of the live (not expired) value stored for `k` without
    /// observable side effects: no TTL refresh, no hit/miss metrics, no lazy
    /// removal of an expired entry. The single-owner counterpart is
    /// [`CachedPeek::cache_peek`](crate::CachedPeek::cache_peek); the sharded stores
    /// return a clone rather than a reference because the value lives behind a
    /// per-shard lock.
    #[must_use]
    pub fn peek(&self, k: &K) -> Option<V> {
        let shard = self.shard_of(k);
        let guard = shard.lock.read();
        guard
            .get(k)
            .filter(|entry| !self.is_expired(entry))
            .map(|entry| entry.value.clone())
    }
}

impl<K, V, H: ShardHasher<K>> ShardedTtlCache<K, V, H>
where
    K: Hash + Eq,
{
    /// Return aggregate metrics across all shards.
    ///
    /// Note: the `size` field includes entries that have expired but not yet been
    /// swept by [`evict`](Self::evict). Call `evict()` first for an accurate live count.
    #[must_use]
    pub fn metrics(&self) -> CacheMetrics {
        let mut hits = 0u64;
        let mut misses = 0u64;
        let mut evictions = 0u64;
        let mut size = 0usize;
        for shard in self.inner.shards.iter() {
            hits += shard.hits.load(Ordering::Relaxed);
            misses += shard.misses.load(Ordering::Relaxed);
            // Evictions are counted per shard (the counter lives on the same cache line the
            // evicting thread already owns) and summed here, exactly like hits/misses.
            evictions += shard.evictions.load(Ordering::Relaxed);
            size += shard.lock.read().len();
        }
        CacheMetrics {
            hits: Some(hits),
            misses: Some(misses),
            evictions: Some(evictions),
            entry_count: Some(size),
            capacity: None,
        }
    }

    /// Number of shards.
    #[must_use]
    pub fn shards(&self) -> usize {
        self.inner.shards.len()
    }

    /// Per-shard live entry counts (including expired-but-not-yet-swept entries).
    #[must_use]
    pub fn shard_sizes(&self) -> Vec<usize> {
        self.inner
            .shards
            .iter()
            .map(|s| s.lock.read().len())
            .collect()
    }

    /// Total number of entries across all shards (including not-yet-swept expired entries).
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.shards.iter().map(|s| s.lock.read().len()).sum()
    }

    /// `true` if no entries are present.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.shards.iter().all(|s| s.lock.read().is_empty())
    }

    /// Remove all entries from every shard. Does **not** fire `on_evict`.
    /// Use [`cache_clear_with_on_evict`](Self::cache_clear_with_on_evict) to opt into callback firing.
    pub fn clear(&self) {
        for shard in self.inner.shards.iter() {
            shard.lock.write().clear();
        }
    }

    /// Remove all entries from every shard, firing `on_evict` for each removed entry when a
    /// callback is configured.
    ///
    /// Unlike [`clear`](Self::clear), every removed entry is counted as an eviction
    /// (`metrics().evictions`) whether or not an `on_evict` callback is configured; the callback
    /// fires only when one is set.
    pub fn cache_clear_with_on_evict(&self) {
        let Some(on_evict) = &self.inner.on_evict else {
            // No callback: only the removed *count* is observable, so clear each shard in
            // place instead of draining every entry into a `Vec` just to drop it.
            for shard in self.inner.shards.iter() {
                let removed = {
                    let mut guard = shard.lock.write();
                    let n = guard.len();
                    guard.clear();
                    n
                };
                if removed > 0 {
                    shard.evictions.fetch_add(removed as u64, Ordering::Relaxed);
                }
            }
            return;
        };
        for shard in self.inner.shards.iter() {
            let removed: Vec<(K, TimedEntry<V>)> = shard.lock.write().drain().collect();
            if !removed.is_empty() {
                shard
                    .evictions
                    .fetch_add(removed.len() as u64, Ordering::Relaxed);
                for (k, entry) in &removed {
                    on_evict(k, &entry.value);
                }
            }
        }
    }

    /// Sweep all shards for expired entries, remove them, fire the `on_evict` callback
    /// (if set) for each, and return the total count of removed entries.
    #[must_use]
    pub fn evict(&self) -> usize
    where
        K: Clone,
    {
        let mut total = 0;
        // Expiry is judged against a single instant sampled once per call, so every entry in
        // every shard is compared against the same instant (matching `retain`).
        let now = Instant::now();
        let Some(cb) = &self.inner.on_evict else {
            // No callback: only the removed *count* is observable, so drop the expired entries
            // in place via `retain` and take the length delta -- no key clones, no `Vec`.
            for shard in self.inner.shards.iter() {
                let removed = {
                    let mut guard = shard.lock.write();
                    let before = guard.len();
                    guard.retain(|_, e| !expired_at(e, now));
                    before - guard.len()
                };
                total += removed;
                if removed > 0 {
                    shard.evictions.fetch_add(removed as u64, Ordering::Relaxed);
                }
            }
            return total;
        };
        for shard in self.inner.shards.iter() {
            // Single pass: `extract_if` removes and yields the expired entries as it walks the
            // table, so no key is cloned and no key is re-hashed for a second lookup.
            // Collect under the write lock, fire callbacks after releasing it.
            let removed: Vec<(K, TimedEntry<V>)> = {
                let mut guard = shard.lock.write();
                guard.extract_if(|_, e| expired_at(e, now)).collect()
            };

            total += removed.len();
            if !removed.is_empty() {
                shard
                    .evictions
                    .fetch_add(removed.len() as u64, Ordering::Relaxed);
                for (k, entry) in &removed {
                    cb(k, &entry.value);
                }
            }
        }
        total
    }

    /// Retain only entries that are unexpired and satisfy `keep`.
    ///
    /// Removes every entry that is already TTL-expired **or** for which `keep` returns
    /// `false` — expired entries are removed without consulting `keep`. `on_evict` is called
    /// and the eviction counter (`metrics().evictions`) incremented for each removed entry.
    /// The single-owner counterpart is [`TtlCache::retain`](crate::TtlCache::retain). This
    /// matches [`ShardedExpiringCache::retain`](crate::ShardedExpiringCache::retain); the
    /// plain [`ShardedUnboundCache::retain`](crate::ShardedUnboundCache::retain) has no
    /// expiry dimension and removes solely on the predicate.
    ///
    /// Expiry is judged against a single instant sampled once at the start of the call, so
    /// every entry in every shard is compared against the same instant.
    ///
    /// Returns the total number of entries removed across all shards for this call, folding
    /// together predicate-rejected entries and entries swept for having already expired -- the
    /// two are not distinguished in the count. Not `#[must_use]`: discarding the count is a
    /// legitimate and common use.
    ///
    /// **Not atomic across shards**: shards are locked and swept **one at a time**, never all
    /// at once (matching [`evict`](Self::evict) and
    /// [`cache_clear_with_on_evict`](Self::cache_clear_with_on_evict)). A concurrent writer
    /// can insert into a shard this call has already visited, and that entry is not filtered.
    ///
    /// `keep` runs while the shard's write lock is held, so it must not re-enter this cache —
    /// the same rule the builder states for `on_evict` — or it will deadlock. `on_evict` fires
    /// **after** the shard lock is released, once per removed entry, in shard order. Because
    /// callbacks run between shard sweeps, an `on_evict` that inserts into a shard this call has
    /// not yet visited will have that entry filtered by the same in-flight `retain`.
    /// # Panicking predicate
    ///
    /// If `keep` panics, nothing has been removed yet from the shard it panicked in (or from
    /// any shard not yet visited): the sweep of a shard runs `keep` in a first pass that only
    /// *selects* doomed entries and removes them in a second pass that runs no user code.
    /// Shards already swept keep their removals, all of which were counted and notified before
    /// the panic. This holds whether or not an `on_evict` callback is configured.
    pub fn retain<F: FnMut(&K, &V) -> bool>(&self, mut keep: F) -> usize {
        let now = Instant::now();
        let mut total_removed = 0usize;
        for shard in self.inner.shards.iter() {
            // Collect under the write lock, fire callbacks after releasing it. Two phases: the
            // first runs `keep` (user code) and only selects, the second removes and runs
            // nothing that can panic. See `stores::take_doomed`. The no-callback path used to
            // take a `before - guard.len()` delta after an in-place `HashMap::retain`, which a
            // panicking predicate skipped entirely; both paths now remove, count, and (where
            // configured) notify exactly the same entries.
            let removed: Vec<(K, TimedEntry<V>)> = {
                let mut guard = shard.lock.write();
                crate::stores::take_doomed(&mut guard, |k, entry| {
                    expired_at(entry, now) || !keep(k, &entry.value)
                })
            };
            total_removed += removed.len();
            if !removed.is_empty() {
                shard
                    .evictions
                    .fetch_add(removed.len() as u64, Ordering::Relaxed);
                if let Some(cb) = &self.inner.on_evict {
                    for (k, entry) in &removed {
                        cb(k, &entry.value);
                    }
                }
            }
        }
        total_removed
    }
}

impl<K, V, H> ConcurrentCacheEvict for ShardedTtlCache<K, V, H>
where
    K: Hash + Eq + Clone,
    H: ShardHasher<K>,
{
    fn evict(&self) -> usize {
        ShardedTtlCache::evict(self)
    }
}

impl<K, V, H> ConcurrentCacheBase for ShardedTtlCache<K, V, H>
where
    K: Hash + Eq,
    V: Clone,
    H: ShardHasher<K>,
{
    type Error = std::convert::Infallible;

    fn cache_size(&self) -> Result<Option<usize>, Self::Error> {
        Ok(Some(self.len()))
    }

    fn cache_hits(&self) -> Option<u64> {
        Some(
            self.inner
                .shards
                .iter()
                .map(|s| s.hits.load(Ordering::Relaxed))
                .sum(),
        )
    }

    fn cache_misses(&self) -> Option<u64> {
        Some(
            self.inner
                .shards
                .iter()
                .map(|s| s.misses.load(Ordering::Relaxed))
                .sum(),
        )
    }

    fn cache_evictions(&self) -> Option<u64> {
        Some(
            self.inner
                .shards
                .iter()
                .map(|s| s.evictions.load(Ordering::Relaxed))
                .sum(),
        )
    }
}

impl<K, V, H> ConcurrentCacheTtl for ShardedTtlCache<K, V, H>
where
    K: Hash + Eq,
    V: Clone,
    H: ShardHasher<K>,
{
    fn ttl(&self) -> Option<Duration> {
        self.ttl_duration()
    }

    fn set_ttl(&self, ttl: Duration) -> Option<Duration> {
        let prev = self
            .inner
            .ttl_nanos
            .swap(encode_ttl(ttl), Ordering::Relaxed);
        decode_ttl(prev)
    }

    fn unset_ttl(&self) -> Option<Duration> {
        let prev = self.inner.ttl_nanos.swap(0, Ordering::Relaxed);
        decode_ttl(prev)
    }
}

impl<K, V, H> ConcurrentCacheRefreshOnHit for ShardedTtlCache<K, V, H>
where
    K: Hash + Eq,
    V: Clone,
    H: ShardHasher<K>,
{
    fn refresh_on_hit(&self) -> bool {
        self.inner.refresh.load(Ordering::Relaxed)
    }

    fn set_refresh_on_hit(&self, refresh: bool) -> bool {
        self.inner.refresh.swap(refresh, Ordering::Relaxed)
    }
}

impl<K, V, H> ConcurrentCached<K, V> for ShardedTtlCache<K, V, H>
where
    K: Hash + Eq,
    V: Clone,
    H: ShardHasher<K>,
{
    fn cache_get(&self, k: &K) -> Result<Option<V>, Self::Error> {
        let shard = self.shard_of(k);
        if self.inner.refresh.load(Ordering::Relaxed) {
            let mut guard = shard.lock.write();
            // The clock is read once, and only when the key is actually present: a lookup for
            // an absent key never touches it. The same instant decides expiry and stamps the
            // renewed `expires_at` (the previous code read the clock twice on this path).
            // `None` = key absent, `Some(None)` = present but expired, `Some(Some(v))` = live
            // hit, already refreshed.
            let outcome: Option<Option<V>> = match guard.get_mut(k) {
                None => None,
                Some(entry) => {
                    let now = Instant::now();
                    if expired_at(entry, now) {
                        Some(None)
                    } else {
                        entry.expires_at = self.compute_expires_at(now).or(entry.expires_at);
                        Some(Some(entry.value.clone()))
                    }
                }
            };
            match outcome {
                Some(Some(value)) => {
                    drop(guard);
                    shard.hits.fetch_add(1, Ordering::Relaxed);
                    return Ok(Some(value));
                }
                Some(None) => {
                    let removed = guard.remove_entry(k);
                    drop(guard);
                    if let Some((stored_k, entry)) = removed {
                        shard.evictions.fetch_add(1, Ordering::Relaxed);
                        if let Some(cb) = &self.inner.on_evict {
                            cb(&stored_k, &entry.value);
                        }
                    }
                    shard.misses.fetch_add(1, Ordering::Relaxed);
                    return Ok(None);
                }
                None => {
                    drop(guard);
                    shard.misses.fetch_add(1, Ordering::Relaxed);
                    return Ok(None);
                }
            }
        }

        // Check for expiry — try with a read lock.
        let (expired, value, now) = {
            let guard = shard.lock.read();
            match guard.get(k) {
                None => {
                    // Release the shard lock before touching the counter, like every other
                    // path in this file. A miss on an absent key never reads the clock.
                    drop(guard);
                    shard.misses.fetch_add(1, Ordering::Relaxed);
                    return Ok(None);
                }
                Some(entry) => {
                    // One clock read per call: this instant is reused by the write-lock
                    // re-check below, which previously sampled the clock a second time.
                    let now = Instant::now();
                    let expired = expired_at(entry, now);
                    let value = if !expired {
                        Some(entry.value.clone())
                    } else {
                        None
                    };
                    (expired, value, now)
                }
            }
        };
        if expired {
            // Upgrade to write lock to remove the expired entry.
            let mut guard = shard.lock.write();
            // Re-check under write lock — another thread may have replaced the entry
            // with a fresh value in the meantime; clone it out in the same lookup.
            let fresh_value = match guard.get(k) {
                Some(entry) if !expired_at(entry, now) => Some(entry.value.clone()),
                _ => None,
            };
            if let Some(fresh_value) = fresh_value {
                drop(guard);
                shard.hits.fetch_add(1, Ordering::Relaxed);
                return Ok(Some(fresh_value));
            }
            // Still expired (or already gone) — remove it.
            let removed = guard.remove_entry(k);
            drop(guard);
            if let Some((stored_k, entry)) = removed {
                shard.evictions.fetch_add(1, Ordering::Relaxed);
                if let Some(cb) = &self.inner.on_evict {
                    cb(&stored_k, &entry.value);
                }
            }
            shard.misses.fetch_add(1, Ordering::Relaxed);
            return Ok(None);
        }
        shard.hits.fetch_add(1, Ordering::Relaxed);
        Ok(value)
    }

    fn cache_set(&self, k: K, v: V) -> Result<Option<V>, Self::Error> {
        let shard = self.shard_of(&k);
        let now = Instant::now();
        let expires_at = self.compute_expires_at(now);
        let new_entry = TimedEntry {
            expires_at,
            value: v,
        };
        // Capture the displaced entry and evaluate expiry while the write lock is still held
        // (B2: avoids a TOCTOU where the entry crosses the expiry threshold between unlock and
        // the check). Expiry is judged against the same `now` that stamps the replacement
        // entry — one clock read per call, and the displaced entry is judged against exactly
        // the instant the new entry claims to start at. An owned key is kept in hand so
        // `on_evict` can fire after the lock is released (on_evict-after-unlock).
        //
        // There is exactly ONE write shape here, taken whether or not an `on_evict` callback is
        // configured, so attaching a purely observational callback cannot change which key is
        // physically stored. An overwrite goes through `remove_entry` + `insert`, rebinding the
        // slot to the CALLER's key (matching the LRU-backed sharded stores) and yielding the
        // displaced stored key owned, so `on_evict` receives the evicted entry's own (key,
        // value) pair after the lock is released. Every sharded `on_evict` site hands the
        // callback the stored pair; a `get_mut` value swap would keep the stored key in the
        // map but leave only the caller's (`Eq`-equal, possibly non-identical) key on hand
        // for the callback. Note the single-owner `TtlCache` differs on the stored-key axis:
        // it keeps the first-inserted key, matching `HashMap::insert`.
        let old: Option<(K, TimedEntry<V>, bool)> = {
            let mut guard = shard.lock.write();
            match guard.remove_entry(&k) {
                Some((stored_k, e)) => {
                    let expired = expired_at(&e, now);
                    guard.insert(k, new_entry);
                    Some((stored_k, e, expired))
                }
                None => {
                    guard.insert(k, new_entry);
                    None
                }
            }
        };
        match old {
            // A displaced expired value is filtered from the return (matching cache_remove and
            // the single-owner TTL stores); fire on_evict and count an eviction for it.
            Some((key, entry, true)) => {
                // Count BEFORE notifying: a panicking callback must never leave an
                // entry removed-but-uncounted.
                shard.evictions.fetch_add(1, Ordering::Relaxed);
                if let Some(cb) = &self.inner.on_evict {
                    cb(&key, &entry.value);
                }
                Ok(None)
            }
            Some((_, entry, false)) => Ok(Some(entry.value)),
            None => Ok(None),
        }
    }

    fn cache_remove(&self, k: &K) -> Result<Option<V>, Self::Error> {
        let shard = self.shard_of(k);
        let removed = shard.lock.write().remove_entry(k);
        if let Some((stored_k, entry)) = removed {
            shard.evictions.fetch_add(1, Ordering::Relaxed);
            if let Some(cb) = &self.inner.on_evict {
                cb(&stored_k, &entry.value);
            }
            // expired = Some(t) and now >= t; None (never-expires) or now < t -> live
            if entry.expires_at.is_some_and(|t| Instant::now() >= t) {
                Ok(None)
            } else {
                Ok(Some(entry.value))
            }
        } else {
            Ok(None)
        }
    }

    fn cache_remove_entry(&self, k: &K) -> Result<Option<(K, V)>, Self::Error> {
        let shard = self.shard_of(k);
        let removed = shard.lock.write().remove_entry(k);
        if let Some((ref stored_k, ref entry)) = removed {
            shard.evictions.fetch_add(1, Ordering::Relaxed);
            if let Some(cb) = &self.inner.on_evict {
                cb(stored_k, &entry.value);
            }
        }
        Ok(removed.map(|(k, entry)| (k, entry.value)))
    }

    fn cache_clear(&self) -> Result<(), Self::Error> {
        self.clear();
        Ok(())
    }

    fn cache_reset(&self) -> Result<(), Self::Error> {
        self.clear();
        ConcurrentCached::cache_reset_metrics(self)
    }

    fn cache_reset_metrics(&self) -> Result<(), Self::Error> {
        for shard in self.inner.shards.iter() {
            shard.hits.store(0, Ordering::Relaxed);
            shard.misses.store(0, Ordering::Relaxed);
            shard.evictions.store(0, Ordering::Relaxed);
        }
        Ok(())
    }

    /// Efficient peek-based contains: acquires a read lock, does not clone the value,
    /// and does not record hit/miss metrics. Returns `true` only for live (not expired) entries.
    fn cache_contains(&self, k: &K) -> Result<bool, Self::Error> {
        let shard = self.shard_of(k);
        let guard = shard.lock.read();
        Ok(guard.get(k).is_some_and(|entry| !self.is_expired(entry)))
    }
}

impl<K, V, H> ConcurrentCachePeek<K, V> for ShardedTtlCache<K, V, H>
where
    K: Hash + Eq,
    V: Clone,
    H: ShardHasher<K>,
{
    fn cache_peek(&self, k: &K) -> Result<Option<V>, Self::Error> {
        Ok(self.peek(k))
    }
}

#[cfg(feature = "async_core")]
#[cfg_attr(docsrs, doc(cfg(feature = "async_core")))]
impl<K, V, H> ConcurrentCachePeekAsync<K, V> for ShardedTtlCache<K, V, H>
where
    K: Hash + Eq + Send + Sync,
    V: Clone + Send + Sync,
    H: ShardHasher<K>,
{
    /// Delegates to the side-effect-free sync [`cache_peek`](ConcurrentCachePeek::cache_peek);
    /// this store never blocks on IO, so there is nothing to await.
    async fn async_cache_peek(&self, k: &K) -> Result<Option<V>, Self::Error> {
        ConcurrentCachePeek::cache_peek(self, k)
    }
}

#[cfg(feature = "async_core")]
#[cfg_attr(docsrs, doc(cfg(feature = "async_core")))]
impl<K, V, H> ConcurrentCachedAsync<K, V> for ShardedTtlCache<K, V, H>
where
    K: Hash + Eq + Send + Sync,
    V: Clone + Send + Sync,
    H: ShardHasher<K>,
{
    async fn async_cache_get(&self, k: &K) -> Result<Option<V>, Self::Error> {
        ConcurrentCached::cache_get(self, k)
    }

    async fn async_cache_set(&self, k: K, v: V) -> Result<Option<V>, Self::Error> {
        ConcurrentCached::cache_set(self, k, v)
    }

    async fn async_cache_remove(&self, k: &K) -> Result<Option<V>, Self::Error> {
        ConcurrentCached::cache_remove(self, k)
    }

    async fn async_cache_remove_entry(&self, k: &K) -> Result<Option<(K, V)>, Self::Error> {
        ConcurrentCached::cache_remove_entry(self, k)
    }

    async fn async_cache_clear(&self) -> Result<(), Self::Error> {
        ConcurrentCached::cache_clear(self)
    }

    async fn async_cache_reset(&self) -> Result<(), Self::Error> {
        ConcurrentCached::cache_reset(self)
    }

    async fn async_cache_reset_metrics(&self) -> Result<(), Self::Error> {
        ConcurrentCached::cache_reset_metrics(self)
    }

    /// Efficient peek-based contains: does not clone the value, does not record hit/miss
    /// metrics, and returns `true` only for live (not expired) entries.
    fn async_cache_contains(&self, k: &K) -> impl Future<Output = Result<bool, Self::Error>> + Send
    where
        Self: Sized + Sync,
        K: Sync,
    {
        let result = ConcurrentCached::cache_contains(self, k);
        async move { result }
    }
}

/// Builder for [`ShardedTtlCache`].
///
/// Unlike the LRU-bounded builders, `ShardedTtlCacheBuilder` has no `per_shard_max_size` method
/// because `ShardedTtlCache` is unbounded in size — entries expire by TTL, not by capacity.
pub struct ShardedTtlCacheBuilder<K, V, H = DefaultShardHasher> {
    shards: Option<usize>,
    per_shard_initial_capacity: Option<usize>,
    ttl: Option<Duration>,
    refresh: bool,
    hasher: Option<H>,
    on_evict: Option<OnEvict<K, V>>,
    _k: std::marker::PhantomData<K>,
    _v: std::marker::PhantomData<V>,
}

impl<K, V> Default for ShardedTtlCacheBuilder<K, V, DefaultShardHasher> {
    fn default() -> Self {
        Self {
            shards: None,
            per_shard_initial_capacity: None,
            ttl: None,
            refresh: false,
            hasher: Some(DefaultShardHasher::default()),
            on_evict: None,
            _k: std::marker::PhantomData,
            _v: std::marker::PhantomData,
        }
    }
}

impl<K, V> ShardedTtlCacheBuilder<K, V> {
    /// Create a builder with default settings. Equivalent to [`ShardedTtlCache::builder`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl<K, V, H> ShardedTtlCacheBuilder<K, V, H> {
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

    /// Set the number of shards (rounded up to the next power of two).
    #[must_use]
    pub fn shards(mut self, shards: usize) -> Self {
        self.shards = Some(shards);
        self
    }

    /// Set the initial allocation capacity of **each shard** (optional, purely a hint).
    ///
    /// Every shard preallocates this many entry slots, so the total preallocation is
    /// `shards × per_shard_initial_capacity`. This is the sharded counterpart of the
    /// single-owner builder's `initial_capacity` (which is a total, since there is
    /// only one map).
    #[must_use]
    pub fn per_shard_initial_capacity(mut self, capacity: usize) -> Self {
        self.per_shard_initial_capacity = Some(capacity);
        self
    }

    /// Set whether cache hits refresh the TTL.
    #[must_use]
    pub fn refresh_on_hit(mut self, refresh: bool) -> Self {
        self.refresh = refresh;
        self
    }

    /// Set a custom shard-selection hasher, changing the type parameter.
    ///
    /// The hasher decides only which shard a key maps to — it does **not** replace the
    /// per-shard store's own internal hashing. Shard selection reads the **upper 32 bits**
    /// of the returned hash (`(hash >> 32) & shard_mask`), so a custom [`ShardHasher`] must
    /// distribute keys across those high bits to avoid lopsided shards; a hasher that only
    /// varies the low 32 bits will pile every key into one shard. See [`ShardHasher`] for the
    /// distribution contract and a worked example. Defaults to [`DefaultShardHasher`].
    #[doc(alias = "with_hasher")]
    #[must_use]
    pub fn hasher<H2: ShardHasher<K>>(self, hasher: H2) -> ShardedTtlCacheBuilder<K, V, H2> {
        ShardedTtlCacheBuilder {
            shards: self.shards,
            per_shard_initial_capacity: self.per_shard_initial_capacity,
            ttl: self.ttl,
            refresh: self.refresh,
            hasher: Some(hasher),
            on_evict: self.on_evict,
            _k: std::marker::PhantomData,
            _v: std::marker::PhantomData,
        }
    }

    /// Set a callback invoked when an entry is evicted. Fires in five situations:
    /// lazily during [`cache_get`](ConcurrentCached::cache_get) when a TTL-expired entry is
    /// found and removed; explicitly via [`evict`](ShardedTtlCache::evict); on
    /// explicit [`cache_remove`](ConcurrentCached::cache_remove); on
    /// [`cache_remove_entry`](ConcurrentCached::cache_remove_entry); and on
    /// [`cache_set`](ConcurrentCached::cache_set) when the displaced entry is already expired.
    /// Does **not** fire on [`clear`](ShardedTtlCache::clear);
    /// use [`cache_clear_with_on_evict`](ShardedTtlCache::cache_clear_with_on_evict) to opt in.
    /// [`cache_clear_with_on_evict`](ShardedTtlCache::cache_clear_with_on_evict) fires
    /// callbacks after releasing the shard lock.
    ///
    /// The closure must be `'static` (its captures cannot borrow from the local stack), but `K`
    /// and `V` themselves are not required to be `'static`.
    #[must_use]
    pub fn on_evict(mut self, on_evict: impl Fn(&K, &V) + Send + Sync + 'static) -> Self {
        self.on_evict = Some(Arc::new(on_evict));
        self
    }

    /// Build the cache.
    ///
    /// Use [`ShardedTtlCache::builder()`] to obtain a builder, set at least
    /// [`ttl`](Self::ttl), then call `.build()`.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError::MissingRequired`] if `ttl` was not set,
    /// [`BuildError::InvalidValue`] if the TTL is zero, or [`BuildError`] if the
    /// shard count overflows.
    #[must_use = "the Result from build() must be used"]
    pub fn build(self) -> Result<ShardedTtlCache<K, V, H>, BuildError>
    where
        K: Hash + Eq,
        H: ShardHasher<K>,
    {
        let ttl = self.ttl.ok_or(BuildError::MissingRequired("ttl"))?;
        crate::stores::validate_ttl(ttl)?;
        let n = checked_shard_count(self.shards)?;
        let mask = n - 1;
        let per_shard_capacity = self.per_shard_initial_capacity.unwrap_or(0);
        let shards = (0..n)
            .map(|_| {
                CachePadded(Shard::new(HashMap::with_capacity_and_hasher(
                    per_shard_capacity,
                    RandomState::new(),
                )))
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Ok(ShardedTtlCache {
            inner: Arc::new(TtlInner {
                shards,
                shard_mask: mask,
                hasher: self
                    .hasher
                    .expect("hasher is always initialized via Default or .hasher()"),
                on_evict: self.on_evict,
                ttl_nanos: AtomicU64::new(encode_ttl(ttl)),
                refresh: AtomicBool::new(self.refresh),
            }),
        })
    }

    /// Build the new cache and copy every non-expired entry from `existing` into it,
    /// preserving the original `TimedEntry` timestamps.
    ///
    /// The target cache uses this builder's TTL setting when checking copied entries.
    /// For the same wall-clock expiry schedule, build the target with the same TTL as
    /// `existing`; a shorter or longer target TTL can make copied entries expire earlier
    /// or later than they would have in the source cache.
    ///
    /// Acquires each shard's read lock on `existing` one at a time. Writes to
    /// `existing` that occur after a shard's read lock is released may or may
    /// not appear in the new cache; the new cache warms up from misses after
    /// the swap.
    ///
    /// **Note**: `on_evict` callbacks on `existing` do not fire — entries are read
    /// (not removed) from the source cache.
    ///
    /// # Errors
    ///
    /// Returns [`Err(BuildError)`](crate::stores::BuildError) if the builder
    /// configuration is invalid (the same conditions as [`build`](Self::build)):
    /// `ttl` was not set or is zero, or the shard count overflows.
    #[must_use = "the Result from copy_from() must be used"]
    pub fn copy_from<H2: ShardHasher<K>>(
        self,
        existing: &ShardedTtlCache<K, V, H2>,
    ) -> Result<ShardedTtlCache<K, V, H>, BuildError>
    where
        K: Clone + Hash + Eq,
        V: Clone,
        H: ShardHasher<K>,
    {
        let new_cache = self.build()?;
        for shard in existing.inner.shards.iter() {
            let entries: Vec<(K, TimedEntry<V>)> = {
                let guard = shard.lock.read();
                let now = Instant::now();
                guard
                    .iter()
                    .filter(|(_, entry)| {
                        // Skip entries that are already expired per their per-entry expires_at.
                        entry.expires_at.is_none_or(|t| now < t)
                    })
                    .map(|(k, e)| (k.clone(), e.clone()))
                    .collect()
            };
            // Insert preserving original timestamps.
            for (k, entry) in entries {
                let new_shard = new_cache.shard_of(&k);
                new_shard.lock.write().insert(k, entry);
            }
        }
        Ok(new_cache)
    }
}

impl<K, V, H> ConcurrentCloneCached<K, V> for ShardedTtlCache<K, V, H>
where
    K: Hash + Eq,
    V: Clone,
    H: ShardHasher<K>,
{
    /// Returns `(Some(v), false)` for a live entry (hit), `(Some(v), true)` for an expired
    /// entry (miss, **no removal**, no eviction counter), or `(None, false)` when absent (miss).
    fn cache_get_with_expiry_status(&self, k: &K) -> (Option<V>, bool) {
        let shard = self.shard_of(k);
        if self.inner.refresh.load(Ordering::Relaxed) {
            // Refresh-on-hit path: write lock needed to update the entry's expires_at.
            let mut guard = shard.lock.write();
            match guard.get_mut(k) {
                None => {
                    drop(guard);
                    shard.misses.fetch_add(1, Ordering::Relaxed);
                    (None, false)
                }
                Some(entry) => {
                    // One clock read, taken only when the key is present: the same instant
                    // decides expiry and stamps the renewed `expires_at`.
                    let now = Instant::now();
                    let expired = expired_at(entry, now);
                    let value = entry.value.clone();
                    if !expired {
                        entry.expires_at = self.compute_expires_at(now).or(entry.expires_at);
                    }
                    drop(guard);
                    if expired {
                        shard.misses.fetch_add(1, Ordering::Relaxed);
                        (Some(value), true)
                    } else {
                        shard.hits.fetch_add(1, Ordering::Relaxed);
                        (Some(value), false)
                    }
                }
            }
        } else {
            // Default path: read lock sufficient; no modification needed.
            let guard = shard.lock.read();
            match guard.get(k) {
                None => {
                    drop(guard);
                    shard.misses.fetch_add(1, Ordering::Relaxed);
                    (None, false)
                }
                Some(entry) => {
                    let expired = expired_at(entry, Instant::now());
                    let value = entry.value.clone();
                    drop(guard);
                    if expired {
                        shard.misses.fetch_add(1, Ordering::Relaxed);
                        (Some(value), true)
                    } else {
                        shard.hits.fetch_add(1, Ordering::Relaxed);
                        (Some(value), false)
                    }
                }
            }
        }
    }

    /// Non-renewing read: takes only a read lock, never updates the TTL timestamp, the
    /// hits/misses counters, or removes the entry. Returns `(Some(v), expired)` for a present
    /// entry (expired or not) or `(None, false)` when absent.
    fn cache_peek_with_expiry_status(&self, k: &K) -> (Option<V>, bool) {
        let shard = self.shard_of(k);
        let guard = shard.lock.read();
        match guard.get(k) {
            None => (None, false),
            Some(entry) => {
                let expired = self.is_expired(entry);
                (Some(entry.value.clone()), expired)
            }
        }
    }
}

impl<K, V, H> ConcurrentCacheExpiry<K, V> for ShardedTtlCache<K, V, H>
where
    K: Hash + Eq,
    H: ShardHasher<K>,
{
    /// Returns the stored value and its expiry instant, with no read side effects.
    ///
    /// Takes only a read lock. The instant is the entry's own deadline, `None` when the entry
    /// never expires (TTL was disabled at insert time). An extreme ttl is clamped to
    /// `u64::MAX` nanoseconds rather than overflowing, so it reports a real far-future
    /// deadline, never `None`. An expired entry is returned with its past deadline and is
    /// **not** removed; the hits/misses counters and the TTL are untouched. The convention is
    /// `now >= t` means expired: a deadline exactly equal to the current instant counts as
    /// already past, matching the liveness check the store itself applies.
    fn cache_peek_expires_at(&self, k: &K) -> (Option<V>, Option<Instant>)
    where
        V: Clone,
    {
        let shard = self.shard_of(k);
        let guard = shard.lock.read();
        match guard.get(k) {
            None => (None, None),
            Some(entry) => (Some(entry.value.clone()), entry.expires_at),
        }
    }

    /// Returns whether the key is present and its expiry instant, without the value.
    ///
    /// The value-free counterpart of
    /// [`cache_peek_expires_at`](ConcurrentCacheExpiry::cache_peek_expires_at): the same shard
    /// read lock and the same deadline, with no clone and no `V: Clone` bound. `(false, None)`
    /// when the key is absent, `(true, None)` when the entry never expires (TTL disabled at
    /// insert time), `(true, Some(t))` otherwise. An extreme ttl is clamped to `u64::MAX`
    /// nanoseconds rather than overflowing, so it reports a real far-future deadline, never
    /// `None`. An expired entry reports `(true, Some(t))` with `t` in the past and is **not**
    /// removed; the hits/misses counters and the TTL are untouched. The convention is
    /// `now >= t` means expired: a deadline exactly equal to the current instant counts as
    /// already past, matching the liveness check the store itself applies.
    fn cache_expires_at(&self, k: &K) -> (bool, Option<Instant>) {
        let shard = self.shard_of(k);
        let guard = shard.lock.read();
        match guard.get(k) {
            None => (false, None),
            Some(entry) => (true, entry.expires_at),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ConcurrentCached;
    use crate::ConcurrentCached as SyncConcurrentCached;
    use crate::ConcurrentCloneCached;

    #[test]
    fn new_returns_ready_cache_respecting_ttl() {
        let c = ShardedTtlCache::<u32, u32>::new(Duration::from_millis(10));
        assert_eq!(c.ttl(), Some(Duration::from_millis(10)));
        assert_eq!(SyncConcurrentCached::cache_set(&c, 1, 100).unwrap(), None);
        assert_eq!(SyncConcurrentCached::cache_get(&c, &1).unwrap(), Some(100));
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert_eq!(
            SyncConcurrentCached::cache_get(&c, &1).unwrap(),
            None,
            "entry must expire after ttl"
        );
    }

    #[test]
    #[should_panic(expected = "non-zero ttl")]
    fn new_zero_ttl_panics() {
        let _c = ShardedTtlCache::<u32, u32>::new(Duration::ZERO);
    }

    #[test]
    fn cache_set_over_expired_returns_none_fires_on_evict_and_counts() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU64, Ordering as AOrd};
        let count = Arc::new(AtomicU64::new(0));
        let count2 = count.clone();
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_millis(20))
            .on_evict(move |_, _| {
                count2.fetch_add(1, AOrd::Relaxed);
            })
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1, 100).unwrap();
        let before = c.metrics().evictions.unwrap();
        std::thread::sleep(std::time::Duration::from_millis(60));
        // Overwriting the expired value: None returned, on_evict fires once, one eviction.
        assert_eq!(SyncConcurrentCached::cache_set(&c, 1, 200).unwrap(), None);
        assert_eq!(c.metrics().evictions.unwrap(), before + 1);
        assert_eq!(count.load(AOrd::Relaxed), 1);
        // Overwriting the now-live value returns it, no on_evict and no new eviction.
        assert_eq!(
            SyncConcurrentCached::cache_set(&c, 1, 300).unwrap(),
            Some(200)
        );
        assert_eq!(c.metrics().evictions.unwrap(), before + 1);
        assert_eq!(count.load(AOrd::Relaxed), 1);
    }

    #[test]
    fn ttl_secs_and_ttl_millis_set_duration() {
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl_secs(7)
            .build()
            .unwrap();
        assert_eq!(c.ttl(), Some(Duration::from_secs(7)));

        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl_millis(250)
            .build()
            .unwrap();
        assert_eq!(c.ttl(), Some(Duration::from_millis(250)));
    }

    #[test]
    fn ttl_setters_override_last_writer_wins() {
        // ttl(secs=10) then ttl_secs(5) -> 5s
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(10))
            .ttl_secs(5)
            .build()
            .unwrap();
        assert_eq!(c.ttl(), Some(Duration::from_secs(5)));

        // ttl_secs then ttl_millis -> the millis value
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl_secs(10)
            .ttl_millis(500)
            .build()
            .unwrap();
        assert_eq!(c.ttl(), Some(Duration::from_millis(500)));
    }

    #[test]
    fn basic_get_set_remove() {
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        assert_eq!(
            SyncConcurrentCached::cache_get(&c, &1).expect("cache_get must succeed"),
            None
        );
        assert_eq!(
            SyncConcurrentCached::cache_set(&c, 1, 100).expect("insert must succeed"),
            None
        );
        assert_eq!(
            SyncConcurrentCached::cache_get(&c, &1).expect("key was just inserted"),
            Some(100)
        );
        assert_eq!(
            SyncConcurrentCached::cache_remove(&c, &1).expect("key must be present"),
            Some(100)
        );
        assert_eq!(
            SyncConcurrentCached::cache_get(&c, &1).expect("cache_get must succeed"),
            None
        );
    }

    #[test]
    fn clone_shares_state() {
        let c1 = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        let c2 = c1.clone();
        SyncConcurrentCached::cache_set(&c1, 1, 10).expect("insert must succeed");
        assert_eq!(
            SyncConcurrentCached::cache_get(&c2, &1).expect("key was just inserted"),
            Some(10)
        );
    }

    #[test]
    fn ttl_expiry() {
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_millis(50))
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1, 100).expect("insert must succeed");
        assert_eq!(
            SyncConcurrentCached::cache_get(&c, &1).expect("key was just inserted"),
            Some(100)
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(
            SyncConcurrentCached::cache_get(&c, &1).expect("cache_get must succeed"),
            None
        );
    }

    #[test]
    fn evict_sweeps_expired() {
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_millis(50))
            .build()
            .unwrap();
        for i in 0..10u32 {
            SyncConcurrentCached::cache_set(&c, i, i).expect("insert must succeed");
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        let removed = c.evict();
        assert_eq!(removed, 10);
        assert_eq!(c.metrics().evictions, Some(10));
    }

    #[test]
    fn set_ttl_inherent() {
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        let prev = c.set_ttl(Duration::from_secs(30));
        assert_eq!(prev, Some(Duration::from_secs(60)));
        assert_eq!(c.ttl(), Some(Duration::from_secs(30)));
    }

    #[test]
    fn try_set_ttl_rejects_zero_and_returns_previous() {
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        // Nonzero: stored, previous ttl returned, and the new ttl takes effect.
        let prev = c.try_set_ttl(Duration::from_secs(30)).unwrap();
        assert_eq!(prev, Some(Duration::from_secs(60)));
        assert_eq!(c.ttl(), Some(Duration::from_secs(30)));
        // Zero: rejected without touching the stored ttl.
        assert_eq!(
            c.try_set_ttl(Duration::ZERO),
            Err(crate::SetTtlError::ZeroTtl)
        );
        assert_eq!(c.ttl(), Some(Duration::from_secs(30)));
    }

    #[test]
    fn copy_from_skips_expired() {
        let old = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_millis(50))
            .build()
            .unwrap();
        for i in 0..10u32 {
            SyncConcurrentCached::cache_set(&old, i, i).expect("insert must succeed");
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        let new_cache = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .copy_from(&old)
            .unwrap();
        // All original entries expired — new cache should be empty
        assert_eq!(new_cache.len(), 0);
    }

    #[test]
    fn copy_from_preserves_live_entries() {
        let old = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        for i in 0..20u32 {
            SyncConcurrentCached::cache_set(&old, i, i * 10).expect("insert must succeed");
        }
        let new_cache = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .copy_from(&old)
            .unwrap();
        for i in 0..20u32 {
            assert_eq!(
                SyncConcurrentCached::cache_get(&new_cache, &i).expect("key was just inserted"),
                Some(i * 10)
            );
        }
    }

    #[test]
    fn send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ShardedTtlCache<u32, u32>>();
    }

    #[test]
    fn build_rejects_zero_ttl() {
        let err = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_nanos(0))
            .build();
        assert!(
            matches!(
                err,
                Err(crate::stores::BuildError::InvalidValue { field: "ttl", .. })
            ),
            "expected InvalidValue, got {err:?}",
        );
    }

    #[test]
    fn cache_clear_with_on_evict_fires_for_all_entries() {
        use std::sync::atomic::{AtomicU64, Ordering};
        let count = Arc::new(AtomicU64::new(0));
        let count2 = count.clone();
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(3600))
            .on_evict(move |_, _| {
                count2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();
        for i in 0..20u32 {
            SyncConcurrentCached::cache_set(&c, i, i).expect("insert must succeed");
        }
        let before = c
            .metrics()
            .evictions
            .expect("eviction-tracking stores report an evictions count");
        c.cache_clear_with_on_evict();
        assert_eq!(
            c.len(),
            0,
            "cache must be empty after cache_clear_with_on_evict"
        );
        assert_eq!(
            count.load(Ordering::Relaxed),
            20,
            "on_evict must fire for every entry"
        );
        assert_eq!(
            c.metrics()
                .evictions
                .expect("eviction-tracking stores report an evictions count")
                - before,
            20,
            "evictions counter must increment for each entry"
        );
    }

    #[test]
    fn cache_clear_with_on_evict_counts_evictions_without_callback() {
        // metrics().evictions must not depend on an on_evict observer being attached.
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(3600))
            .build()
            .unwrap();
        for i in 0..20u32 {
            SyncConcurrentCached::cache_set(&c, i, i).expect("insert must succeed");
        }
        let before = c.metrics().evictions.expect("evictions tracked");
        c.cache_clear_with_on_evict();
        assert_eq!(c.len(), 0);
        assert_eq!(
            c.metrics().evictions.expect("evictions tracked") - before,
            20,
            "evictions must be counted even with no on_evict callback"
        );
        for i in 0..5u32 {
            SyncConcurrentCached::cache_set(&c, i, i).expect("insert must succeed");
        }
        let before_plain = c.metrics().evictions.expect("evictions tracked");
        c.clear();
        assert_eq!(
            c.metrics().evictions.expect("evictions tracked"),
            before_plain,
            "plain clear() must not count evictions"
        );
    }

    #[test]
    fn clear_does_not_fire_on_evict() {
        use std::sync::atomic::{AtomicU64, Ordering};
        let count = Arc::new(AtomicU64::new(0));
        let count2 = count.clone();
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(3600))
            .on_evict(move |_, _| {
                count2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();
        for i in 0..10u32 {
            SyncConcurrentCached::cache_set(&c, i, i).expect("insert must succeed");
        }
        c.clear();
        assert_eq!(
            count.load(Ordering::Relaxed),
            0,
            "clear must not fire on_evict"
        );
    }

    #[test]
    fn retain_fires_on_evict_after_the_shard_lock_is_released() {
        // The callback must observe every shard lock as free: `retain` collects the
        // removed entries under the shard write guard, drops it, and only then fires
        // `on_evict`. `try_write` returning `None` for any shard would mean the guard
        // was still held while the callback ran.
        use std::sync::OnceLock;
        use std::sync::atomic::{AtomicU64, Ordering};

        let handle: Arc<OnceLock<ShardedTtlCache<u32, u32>>> = Arc::new(OnceLock::new());
        let handle2 = handle.clone();
        let fired = Arc::new(AtomicU64::new(0));
        let fired2 = fired.clone();
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(3600))
            .shards(4)
            .on_evict(move |_, _| {
                let cache = handle2.get().expect("handle is set before retain runs");
                assert!(
                    cache
                        .inner
                        .shards
                        .iter()
                        .all(|s| s.lock.try_write().is_some()),
                    "on_evict must fire after the shard write lock is released"
                );
                fired2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();
        handle.set(c.clone()).expect("handle set once");
        for i in 0..32u32 {
            SyncConcurrentCached::cache_set(&c, i, i).expect("insert must succeed");
        }
        c.retain(|_k, _v| false);
        assert_eq!(c.len(), 0, "a keep-nothing predicate empties every shard");
        assert_eq!(
            fired.load(Ordering::Relaxed),
            32,
            "on_evict fires exactly once per removed entry"
        );
    }

    #[test]
    fn retain_judges_expiry_against_a_single_sampled_instant() {
        // Expiry is sampled once per call, so an entry whose `expires_at` is in the
        // future relative to that sample survives, and one already past it does not —
        // no per-entry re-sampling of the clock.
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(3600))
            .shards(2)
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1, 10).expect("insert must succeed");
        SyncConcurrentCached::cache_set(&c, 2, 20).expect("insert must succeed");
        // Backdate key 1's expiry directly in its shard so it is expired without sleeping:
        // `retain` samples its own `now` after this line, and `now >= expires_at` is expired.
        {
            let shard = c.shard_of(&1);
            let mut guard = shard.lock.write();
            let entry = guard.get_mut(&1).expect("key 1 stored");
            entry.expires_at = Some(Instant::now());
        }
        let before = c.metrics().evictions.expect("ttl store tracks evictions");
        c.retain(|_k, _v| true);
        assert_eq!(
            c.len(),
            1,
            "the backdated entry is removed despite keep=true"
        );
        assert_eq!(SyncConcurrentCached::cache_get(&c, &2).unwrap(), Some(20));
        assert_eq!(
            c.metrics().evictions.expect("ttl store tracks evictions") - before,
            1
        );
    }

    #[test]
    fn retain_and_evict_agree_at_the_expires_at_equals_now_boundary() {
        // Both `retain` and `evict` decide expiry with the same `now >= expires_at`
        // comparison. The existing `retain_judges_expiry_against_a_single_sampled_instant`
        // test only backdates an entry (`expires_at` clearly in the past); this test pins
        // the boundary itself -- an entry whose `expires_at` is set to (approximately) the
        // instant just sampled -- and checks `evict()` and `retain()` remove it identically.
        // Because `Instant` is monotonic, the `now` sampled a moment later inside `evict`/
        // `retain` is always `>=` the `expires_at` captured here, so both paths must treat
        // it as expired.
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(3600))
            .shards(1)
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1, 10).expect("insert must succeed");
        SyncConcurrentCached::cache_set(&c, 2, 20).expect("insert must succeed");

        // evict() path: pin key 1's expiry to "now".
        {
            let shard = c.shard_of(&1);
            let mut guard = shard.lock.write();
            let entry = guard.get_mut(&1).expect("key 1 stored");
            entry.expires_at = Some(Instant::now());
        }
        let removed = c.evict();
        assert_eq!(
            removed, 1,
            "an entry whose expires_at is the just-sampled now must be swept by evict()"
        );
        assert_eq!(SyncConcurrentCached::cache_get(&c, &1).unwrap(), None);
        assert_eq!(SyncConcurrentCached::cache_get(&c, &2).unwrap(), Some(20));

        // retain() path: symmetric case with a fresh entry pinned the same way.
        {
            let shard = c.shard_of(&2);
            let mut guard = shard.lock.write();
            let entry = guard.get_mut(&2).expect("key 2 stored");
            entry.expires_at = Some(Instant::now());
        }
        c.retain(|_k, _v| true);
        assert_eq!(
            c.len(),
            0,
            "the same now-boundary entry must be swept by retain() too, agreeing with evict()"
        );
    }

    #[test]
    fn cache_remove_entry_returns_some_for_live_entry() {
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1u32, 100u32).expect("insert must succeed");
        assert_eq!(
            SyncConcurrentCached::cache_remove_entry(&c, &999u32)
                .expect("cache_remove_entry must succeed"),
            None
        );
        assert_eq!(
            SyncConcurrentCached::cache_remove_entry(&c, &1u32).expect("key must be present"),
            Some((1u32, 100u32))
        );
        assert_eq!(
            SyncConcurrentCached::cache_get(&c, &1u32).expect("cache_get must succeed"),
            None
        );
    }

    #[test]
    fn cache_remove_entry_returns_some_for_expired_entry() {
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_millis(50))
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1u32, 100u32).expect("insert must succeed");
        SyncConcurrentCached::cache_set(&c, 2u32, 200u32).expect("insert must succeed");
        std::thread::sleep(std::time::Duration::from_millis(100));

        // cache_remove returns None for expired.
        assert_eq!(
            SyncConcurrentCached::cache_remove(&c, &1u32).expect("cache_remove must succeed"),
            None
        );

        // cache_remove_entry returns Some even for expired.
        let removed =
            SyncConcurrentCached::cache_remove_entry(&c, &2u32).expect("key must be present");
        assert!(
            removed.is_some(),
            "cache_remove_entry must return Some for expired entry"
        );
        assert_eq!(removed.expect("must be Some"), (2u32, 200u32));
    }

    #[test]
    fn cache_delete_returns_true_for_expired_entry() {
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_millis(50))
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1u32, 100u32).expect("insert must succeed");
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(
            SyncConcurrentCached::cache_delete(&c, &1u32).expect("cache_delete must succeed"),
            "cache_delete must be true for expired entry"
        );
        assert!(!SyncConcurrentCached::cache_delete(&c, &1u32).expect("cache_delete must succeed"));
    }

    #[test]
    fn cache_remove_entry_fires_on_evict_for_expired() {
        use std::sync::atomic::{AtomicU64, Ordering};
        let count = Arc::new(AtomicU64::new(0));
        let count2 = count.clone();
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_millis(50))
            .shards(1)
            .on_evict(move |_, _| {
                count2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1u32, 10u32).expect("insert must succeed");
        std::thread::sleep(std::time::Duration::from_millis(100));

        SyncConcurrentCached::cache_remove_entry(&c, &1u32).expect("key must be present");
        assert_eq!(
            count.load(Ordering::Relaxed),
            1,
            "on_evict fires for expired entries"
        );

        SyncConcurrentCached::cache_remove_entry(&c, &999u32)
            .expect("cache_remove_entry must succeed");
        assert_eq!(count.load(Ordering::Relaxed), 1, "no fire for absent key");
    }

    #[test]
    fn cache_remove_entry_increments_eviction_counter() {
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_millis(10))
            .shards(1)
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1u32, 10u32).expect("insert must succeed");
        std::thread::sleep(std::time::Duration::from_millis(100));
        let before = c.metrics().evictions.expect("evictions are always tracked");
        SyncConcurrentCached::cache_remove_entry(&c, &1u32).expect("key must be present"); // expired but present — must increment
        SyncConcurrentCached::cache_remove_entry(&c, &999u32)
            .expect("cache_remove_entry must succeed"); // absent — must not increment
        assert_eq!(
            c.metrics().evictions.expect("evictions are always tracked") - before,
            1,
            "cache_remove_entry must increment evictions for present key only"
        );
    }

    // --- ConcurrentCloneCached tests ---

    #[test]
    fn concurrent_clone_cached_absent_is_none_false() {
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        assert_eq!(
            ConcurrentCloneCached::cache_get_with_expiry_status(&c, &1u32),
            (None, false),
            "absent key must return (None, false)"
        );
        assert_eq!(
            c.metrics().misses,
            Some(1),
            "absent lookup must increment misses"
        );
    }

    #[test]
    fn concurrent_clone_cached_live_entry_is_some_false() {
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1u32, 42u32).expect("insert must succeed");
        assert_eq!(
            ConcurrentCloneCached::cache_get_with_expiry_status(&c, &1u32),
            (Some(42), false),
            "live entry must return (Some(v), false)"
        );
        assert_eq!(c.metrics().hits, Some(1), "live lookup must increment hits");
        assert_eq!(
            c.metrics().evictions,
            Some(0),
            "live lookup must not increment evictions"
        );
    }

    #[test]
    fn concurrent_clone_cached_expired_returns_stale_no_eviction() {
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_millis(50))
            .shards(1)
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1u32, 99u32).expect("insert must succeed");
        std::thread::sleep(std::time::Duration::from_millis(100));

        let (val, expired) = ConcurrentCloneCached::cache_get_with_expiry_status(&c, &1u32);
        assert_eq!(val, Some(99), "expired entry must return the stale value");
        assert!(expired, "expired entry must set the expired flag");
        assert_eq!(
            c.metrics().misses,
            Some(1),
            "expired lookup must increment misses"
        );
        assert_eq!(
            c.metrics().evictions,
            Some(0),
            "expired lookup must NOT increment evictions"
        );

        // The entry must NOT have been removed — a regular cache_get still sees it.
        // (cache_get will evict it, hence the separate assertion above.)
        let (val2, expired2) = ConcurrentCloneCached::cache_get_with_expiry_status(&c, &1u32);
        assert_eq!(
            val2,
            Some(99),
            "entry must still be present after expiry-status lookup"
        );
        assert!(
            expired2,
            "entry must still be expired on second expiry-status call"
        );
    }

    #[test]
    fn peek_with_expiry_status_no_side_effects() {
        // Build a 1-shard cache so metrics are not split across shards, making
        // counter captures exact.
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .shards(1)
            .build()
            .unwrap();

        SyncConcurrentCached::cache_set(&c, 1u32, 42u32).expect("insert must succeed");

        // Capture counters before any peek.
        let before = c.metrics();

        // Live key: expect (Some(42), false).
        let (val, expired) = ConcurrentCloneCached::cache_peek_with_expiry_status(&c, &1u32);
        assert_eq!(val, Some(42), "live peek must return the value");
        assert!(!expired, "live peek must report expired=false");

        // Absent key: expect (None, false).
        let (val2, expired2) = ConcurrentCloneCached::cache_peek_with_expiry_status(&c, &999u32);
        assert!(val2.is_none(), "absent peek must return None");
        assert!(!expired2, "absent peek must report expired=false");

        // Counters must be unchanged.
        let after = c.metrics();
        assert_eq!(after.hits, before.hits, "peek must not increment hits");
        assert_eq!(
            after.misses, before.misses,
            "peek must not increment misses"
        );
        assert_eq!(
            after.evictions, before.evictions,
            "peek must not increment evictions"
        );
        // Entry must still be present.
        assert_eq!(
            SyncConcurrentCached::cache_get(&c, &1u32).expect("cache_get must succeed"),
            Some(42),
            "entry must still be present after peek"
        );
    }

    #[test]
    fn peek_with_expiry_status_stale_entry_no_side_effects() {
        // Insert an entry with a very short TTL, let it expire, then peek it.
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_millis(10))
            .shards(1)
            .build()
            .unwrap();

        SyncConcurrentCached::cache_set(&c, 1u32, 77u32).expect("insert must succeed");
        std::thread::sleep(std::time::Duration::from_millis(50));

        let before = c.metrics();

        let (val, expired) = ConcurrentCloneCached::cache_peek_with_expiry_status(&c, &1u32);
        assert_eq!(val, Some(77), "expired peek must return the stale value");
        assert!(expired, "expired peek must report expired=true");

        // Counters must be unchanged.
        let after = c.metrics();
        assert_eq!(
            after.hits, before.hits,
            "expired peek must not increment hits"
        );
        assert_eq!(
            after.misses, before.misses,
            "expired peek must not increment misses"
        );
        assert_eq!(
            after.evictions, before.evictions,
            "expired peek must not increment evictions"
        );

        // Entry must NOT have been removed by the peek.
        let (val2, expired2) = ConcurrentCloneCached::cache_peek_with_expiry_status(&c, &1u32);
        assert_eq!(
            val2,
            Some(77),
            "entry must still be present after expired peek"
        );
        assert!(expired2, "entry must still be expired after peek");
    }

    #[test]
    fn peek_with_expiry_status_does_not_renew_ttl_under_refresh_on_hit() {
        // peek must not extend the TTL even when refresh_on_hit is enabled.
        let c = ShardedTtlCache::<u32, u32>::builder()
            .refresh_on_hit(true)
            .ttl(Duration::from_millis(10))
            .shards(1)
            .build()
            .unwrap();

        SyncConcurrentCached::cache_set(&c, 1u32, 42u32).expect("insert must succeed");

        // Entry is live; peek must return the value and report not-expired.
        let (val, expired) = ConcurrentCloneCached::cache_peek_with_expiry_status(&c, &1u32);
        assert_eq!(val, Some(42), "live peek must return the value");
        assert!(!expired, "live peek must report expired=false");

        // Wait past the original TTL.
        std::thread::sleep(std::time::Duration::from_millis(50));

        // If peek had renewed the TTL the entry would still be live; it must not have.
        let (val2, expired2) = ConcurrentCloneCached::cache_peek_with_expiry_status(&c, &1u32);
        assert_eq!(
            val2,
            Some(42),
            "post-sleep peek must still return the value"
        );
        assert!(
            expired2,
            "peek must not renew TTL; entry must now be expired"
        );
    }

    // --- Inherent infallible method tests ---

    #[test]
    fn inherent_get_returns_option_not_result() {
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        let v: Option<u32> = c.get(&1);
        assert_eq!(v, None);
        c.set(1, 42);
        let v: Option<u32> = c.get(&1);
        assert_eq!(v, Some(42));
    }

    #[test]
    fn inherent_set_returns_previous_value() {
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        let prev: Option<u32> = c.set(1, 10);
        assert_eq!(prev, None);
        let prev: Option<u32> = c.set(1, 20);
        assert_eq!(prev, Some(10));
        assert_eq!(c.get(&1), Some(20));
    }

    #[test]
    fn inherent_remove_returns_prior_value() {
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        c.set(1, 99);
        let v: Option<u32> = c.remove(&1);
        assert_eq!(v, Some(99));
        assert_eq!(c.remove(&1), None);
        assert_eq!(c.get(&1), None);
    }

    #[test]
    fn inherent_remove_entry_returns_key_and_value() {
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        c.set(7, 77);
        let pair: Option<(u32, u32)> = c.remove_entry(&7);
        assert_eq!(pair, Some((7, 77)));
        assert_eq!(c.remove_entry(&7), None);
    }

    #[test]
    fn inherent_delete_returns_bool() {
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        c.set(1, 10);
        let removed: bool = c.delete(&1);
        assert!(removed);
        let removed: bool = c.delete(&1);
        assert!(!removed);
    }

    #[test]
    fn inherent_reset_clears_and_resets_metrics() {
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        c.set(1, 1);
        c.set(2, 2);
        let _ = c.get(&1);
        assert_eq!(c.len(), 2);
        assert_eq!(c.metrics().hits, Some(1));
        c.reset();
        assert_eq!(c.len(), 0);
        assert!(c.is_empty());
        assert_eq!(c.metrics().hits, Some(0));
    }

    #[test]
    fn inherent_and_trait_methods_coexist_via_fully_qualified_path() {
        fn use_trait<C>(cache: &C, k: u32, v: u32)
        where
            C: SyncConcurrentCached<u32, u32>,
        {
            let _: Result<Option<u32>, _> = ConcurrentCached::cache_set(cache, k, v);
            let _: Result<Option<u32>, _> = ConcurrentCached::cache_get(cache, &k);
            let _: Result<Option<u32>, _> = ConcurrentCached::cache_remove(cache, &k);
        }
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        use_trait(&c, 1, 100);
    }

    // B2 regression: expiry is evaluated while the write lock is held, so the decision
    // to filter the displaced entry and fire on_evict is made from the state observed
    // under the lock rather than from a later (possibly different) `Instant::now()`.
    #[test]
    fn displaced_expired_entry_skips_return_fires_on_evict_and_counts() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU64, Ordering as AOrd};
        let fired = Arc::new(AtomicU64::new(0));
        let fired2 = fired.clone();
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_millis(20))
            .on_evict(move |_, _| {
                fired2.fetch_add(1, AOrd::Relaxed);
            })
            .build()
            .unwrap();
        // Insert an entry and let it expire.
        SyncConcurrentCached::cache_set(&c, 1, 100).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(60));
        let before = c.metrics().evictions.unwrap();
        // Overwriting the expired entry: must return None, fire on_evict, and count eviction.
        let result = SyncConcurrentCached::cache_set(&c, 1, 200).unwrap();
        assert_eq!(result, None, "displaced expired entry must not be returned");
        assert_eq!(
            c.metrics().evictions.unwrap(),
            before + 1,
            "eviction counter must increment for displaced expired entry"
        );
        assert_eq!(
            fired.load(AOrd::Relaxed),
            1,
            "on_evict must fire exactly once for the displaced expired entry"
        );
        // Overwriting the now-live entry: must return the value, no new on_evict.
        let before2 = c.metrics().evictions.unwrap();
        let result2 = SyncConcurrentCached::cache_set(&c, 1, 300).unwrap();
        assert_eq!(result2, Some(200), "displaced live entry must be returned");
        assert_eq!(
            c.metrics().evictions.unwrap(),
            before2,
            "overwriting a live entry must not increment evictions"
        );
        assert_eq!(
            fired.load(AOrd::Relaxed),
            1,
            "on_evict must not fire again for a displaced live entry"
        );
    }

    // --- Per-shard eviction counter -----------------------------------------
    //
    // Evictions are counted on the shard that owns the key (`Shard::evictions`) and summed
    // on read by `metrics()` / `cache_evictions()`, rather than on one process-wide counter
    // in `Arc<TtlInner>`. These tests pin both halves: the placement (which shard's counter
    // moved) and the aggregation (the sum the public API reports).

    /// Raw per-shard eviction counters, in shard order.
    fn shard_eviction_counters<K, V, H>(c: &ShardedTtlCache<K, V, H>) -> Vec<u64> {
        c.inner
            .shards
            .iter()
            .map(|s| s.evictions.load(Ordering::Relaxed))
            .collect()
    }

    /// Index of the shard that owns `k`.
    fn owning_shard<K, V, H: ShardHasher<K>>(c: &ShardedTtlCache<K, V, H>, k: &K) -> usize {
        shard_index(c.inner.hasher.shard_hash(k), c.inner.shard_mask)
    }

    #[test]
    fn evict_counts_land_on_owning_shards_and_aggregate_exactly() {
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_millis(1))
            .shards(8)
            .build()
            .unwrap();
        for i in 0..64u32 {
            SyncConcurrentCached::cache_set(&c, i, i).expect("insert must succeed");
        }
        std::thread::sleep(std::time::Duration::from_millis(30));
        assert_eq!(c.evict(), 64, "every entry is expired");

        let per_shard = shard_eviction_counters(&c);
        assert_eq!(per_shard.len(), 8, "one counter per shard");
        assert_eq!(
            per_shard.iter().sum::<u64>(),
            64,
            "per-shard counters must account for every eviction: {per_shard:?}"
        );
        assert!(
            per_shard.iter().filter(|&&n| n > 0).count() >= 2,
            "64 keys over 8 shards must move more than one shard's counter: {per_shard:?}"
        );
        assert_eq!(
            c.metrics().evictions,
            Some(64),
            "metrics() must sum the per-shard counters"
        );
        assert_eq!(
            ConcurrentCacheBase::cache_evictions(&c),
            Some(64),
            "cache_evictions() must sum the per-shard counters"
        );
    }

    #[test]
    fn every_eviction_path_counts_on_the_owning_shard() {
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_millis(20))
            .shards(8)
            .build()
            .unwrap();
        let mut expected = vec![0u64; 8];

        // 1) Lazy expiry through cache_get.
        SyncConcurrentCached::cache_set(&c, 1, 10).expect("insert must succeed");
        std::thread::sleep(std::time::Duration::from_millis(60));
        assert_eq!(SyncConcurrentCached::cache_get(&c, &1).unwrap(), None);
        expected[owning_shard(&c, &1)] += 1;
        assert_eq!(
            shard_eviction_counters(&c),
            expected,
            "cache_get lazy expiry must count on the key's own shard"
        );

        // 2) cache_remove.
        SyncConcurrentCached::cache_set(&c, 2, 20).expect("insert must succeed");
        assert_eq!(
            SyncConcurrentCached::cache_remove(&c, &2).unwrap(),
            Some(20)
        );
        expected[owning_shard(&c, &2)] += 1;
        assert_eq!(
            shard_eviction_counters(&c),
            expected,
            "cache_remove must count on the key's own shard"
        );

        // 3) cache_remove_entry.
        SyncConcurrentCached::cache_set(&c, 3, 30).expect("insert must succeed");
        assert_eq!(
            SyncConcurrentCached::cache_remove_entry(&c, &3).unwrap(),
            Some((3, 30))
        );
        expected[owning_shard(&c, &3)] += 1;
        assert_eq!(
            shard_eviction_counters(&c),
            expected,
            "cache_remove_entry must count on the key's own shard"
        );

        // 4) cache_set displacing an expired entry.
        SyncConcurrentCached::cache_set(&c, 4, 40).expect("insert must succeed");
        std::thread::sleep(std::time::Duration::from_millis(60));
        assert_eq!(SyncConcurrentCached::cache_set(&c, 4, 41).unwrap(), None);
        expected[owning_shard(&c, &4)] += 1;
        assert_eq!(
            shard_eviction_counters(&c),
            expected,
            "cache_set over an expired entry must count on the key's own shard"
        );

        // 5) evict() sweeps key 4 (re-set above) and key 5, both expired by now.
        SyncConcurrentCached::cache_set(&c, 5, 50).expect("insert must succeed");
        std::thread::sleep(std::time::Duration::from_millis(60));
        assert_eq!(c.evict(), 2, "both stored entries are expired");
        expected[owning_shard(&c, &4)] += 1;
        expected[owning_shard(&c, &5)] += 1;
        assert_eq!(
            shard_eviction_counters(&c),
            expected,
            "evict must count each removal on the shard it came from"
        );

        // 6) retain().
        SyncConcurrentCached::cache_set(&c, 6, 60).expect("insert must succeed");
        c.retain(|_k, _v| false);
        expected[owning_shard(&c, &6)] += 1;
        assert_eq!(
            shard_eviction_counters(&c),
            expected,
            "retain must count each removal on the shard it came from"
        );

        // 7) cache_clear_with_on_evict (no callback configured: the early-return path).
        SyncConcurrentCached::cache_set(&c, 7, 70).expect("insert must succeed");
        SyncConcurrentCached::cache_set(&c, 8, 80).expect("insert must succeed");
        c.cache_clear_with_on_evict();
        expected[owning_shard(&c, &7)] += 1;
        expected[owning_shard(&c, &8)] += 1;
        assert_eq!(
            shard_eviction_counters(&c),
            expected,
            "cache_clear_with_on_evict must count each removal on its own shard"
        );

        let total: u64 = expected.iter().sum();
        assert_eq!(
            c.metrics().evictions,
            Some(total),
            "metrics() must report the sum of the per-shard counters"
        );
        assert_eq!(ConcurrentCacheBase::cache_evictions(&c), Some(total));
    }

    #[test]
    fn cache_reset_metrics_zeroes_the_per_shard_eviction_counters() {
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_millis(1))
            .shards(4)
            .build()
            .unwrap();
        for i in 0..16u32 {
            SyncConcurrentCached::cache_set(&c, i, i).expect("insert must succeed");
        }
        std::thread::sleep(std::time::Duration::from_millis(30));
        assert_eq!(c.evict(), 16);
        assert_eq!(c.metrics().evictions, Some(16));
        assert_eq!(
            shard_eviction_counters(&c).iter().sum::<u64>(),
            16,
            "the counts must live on the shards before the reset can zero them"
        );
        ConcurrentCached::cache_reset_metrics(&c).unwrap();
        assert_eq!(
            shard_eviction_counters(&c),
            vec![0u64; 4],
            "every per-shard counter must be zeroed"
        );
        assert_eq!(c.metrics().evictions, Some(0));
        assert_eq!(ConcurrentCacheBase::cache_evictions(&c), Some(0));
    }

    #[test]
    fn deep_clone_carries_per_shard_eviction_counts_and_then_diverges() {
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_millis(1))
            .shards(4)
            .build()
            .unwrap();
        for i in 0..32u32 {
            SyncConcurrentCached::cache_set(&c, i, i).expect("insert must succeed");
        }
        std::thread::sleep(std::time::Duration::from_millis(30));
        assert_eq!(c.evict(), 32);
        let source_counters = shard_eviction_counters(&c);
        assert_eq!(
            source_counters.iter().sum::<u64>(),
            32,
            "the source must hold its counts on its own shards: {source_counters:?}"
        );

        let d = c.deep_clone();
        assert_eq!(
            shard_eviction_counters(&d),
            source_counters,
            "deep_clone must copy the counters shard-for-shard, not just the total"
        );
        assert_eq!(
            d.metrics().evictions,
            Some(32),
            "the aggregate must survive deep_clone"
        );
        assert_eq!(ConcurrentCacheBase::cache_evictions(&d), Some(32));

        // The copy is independent: further evictions on the source do not move the clone.
        SyncConcurrentCached::cache_set(&c, 100, 1).expect("insert must succeed");
        assert!(SyncConcurrentCached::cache_delete(&c, &100).unwrap());
        assert_eq!(c.metrics().evictions, Some(33));
        assert_eq!(d.metrics().evictions, Some(32));
        assert_eq!(shard_eviction_counters(&d), source_counters);
    }

    // --- evict(): one-pass sweep, both branches ------------------------------

    #[test]
    fn evict_with_callback_fires_once_per_removed_entry_with_key_and_value() {
        use std::sync::Mutex;
        let seen: Arc<Mutex<Vec<(u32, u32)>>> = Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_millis(30))
            .shards(4)
            .on_evict(move |k: &u32, v: &u32| {
                seen2.lock().expect("callback lock").push((*k, *v));
            })
            .build()
            .unwrap();
        for i in 0..16u32 {
            SyncConcurrentCached::cache_set(&c, i, i * 10).expect("insert must succeed");
        }
        std::thread::sleep(std::time::Duration::from_millis(80));
        // Fresh entries inserted after the sleep must survive the sweep.
        for i in 100..104u32 {
            SyncConcurrentCached::cache_set(&c, i, i).expect("insert must succeed");
        }

        let removed = c.evict();
        assert_eq!(
            removed, 16,
            "evict must return the number of removed entries"
        );

        let mut fired = seen.lock().expect("callback lock").clone();
        fired.sort_unstable();
        let expected: Vec<(u32, u32)> = (0..16u32).map(|i| (i, i * 10)).collect();
        assert_eq!(
            fired, expected,
            "on_evict must fire exactly once per removed entry, with its own key and value"
        );
        assert_eq!(c.len(), 4, "live entries must survive the sweep");
        for i in 100..104u32 {
            assert_eq!(SyncConcurrentCached::cache_get(&c, &i).unwrap(), Some(i));
        }
        assert_eq!(c.metrics().evictions, Some(16));
        assert_eq!(
            shard_eviction_counters(&c).iter().sum::<u64>(),
            16,
            "the callback branch must count on the per-shard counters"
        );
    }

    #[test]
    fn evict_without_callback_returns_count_and_counts_evictions() {
        // The no-callback branch removes in place (`retain` + length delta) and never builds
        // a Vec, so the returned count comes from the length delta -- pin it here.
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_millis(30))
            .shards(4)
            .build()
            .unwrap();
        for i in 0..16u32 {
            SyncConcurrentCached::cache_set(&c, i, i * 10).expect("insert must succeed");
        }
        std::thread::sleep(std::time::Duration::from_millis(80));
        for i in 100..104u32 {
            SyncConcurrentCached::cache_set(&c, i, i).expect("insert must succeed");
        }

        let removed = c.evict();
        assert_eq!(
            removed, 16,
            "the length-delta count must match the removed entries"
        );
        assert_eq!(c.len(), 4, "live entries must survive the sweep");
        for i in 100..104u32 {
            assert_eq!(SyncConcurrentCached::cache_get(&c, &i).unwrap(), Some(i));
        }
        for i in 0..16u32 {
            assert_eq!(c.peek(&i), None, "expired entries must be physically gone");
        }
        assert_eq!(c.metrics().evictions, Some(16));
        assert_eq!(
            shard_eviction_counters(&c).iter().sum::<u64>(),
            16,
            "the no-callback branch must count on the per-shard counters"
        );
    }

    #[test]
    fn evict_with_nothing_expired_removes_nothing_in_either_branch() {
        for with_callback in [false, true] {
            let fired = Arc::new(std::sync::atomic::AtomicU64::new(0));
            let fired2 = fired.clone();
            let mut builder = ShardedTtlCache::<u32, u32>::builder()
                .ttl(Duration::from_secs(3600))
                .shards(4);
            if with_callback {
                builder = builder.on_evict(move |_k: &u32, _v: &u32| {
                    fired2.fetch_add(1, Ordering::Relaxed);
                });
            }
            let c = builder.build().unwrap();
            for i in 0..16u32 {
                SyncConcurrentCached::cache_set(&c, i, i).expect("insert must succeed");
            }
            assert_eq!(
                c.evict(),
                0,
                "nothing is expired (with_callback={with_callback})"
            );
            assert_eq!(c.len(), 16, "no entry may be dropped by a no-op sweep");
            assert_eq!(c.metrics().evictions, Some(0));
            assert_eq!(shard_eviction_counters(&c), vec![0u64; 4]);
            assert_eq!(fired.load(Ordering::Relaxed), 0, "on_evict must not fire");
        }
    }

    #[test]
    fn evict_removes_only_expired_entries_in_either_branch() {
        for with_callback in [false, true] {
            let fired = Arc::new(std::sync::atomic::AtomicU64::new(0));
            let fired2 = fired.clone();
            let mut builder = ShardedTtlCache::<u32, u32>::builder()
                .ttl(Duration::from_secs(3600))
                .shards(2);
            if with_callback {
                builder = builder.on_evict(move |_k: &u32, _v: &u32| {
                    fired2.fetch_add(1, Ordering::Relaxed);
                });
            }
            let c = builder.build().unwrap();
            for i in 0..8u32 {
                SyncConcurrentCached::cache_set(&c, i, i).expect("insert must succeed");
            }
            // Backdate the even keys so exactly half the entries are expired, without sleeping.
            let past = Instant::now();
            for i in (0..8u32).step_by(2) {
                let shard = c.shard_of(&i);
                let mut guard = shard.lock.write();
                guard.get_mut(&i).expect("key stored").expires_at = Some(past);
            }
            assert_eq!(c.evict(), 4, "(with_callback={with_callback})");
            assert_eq!(c.len(), 4);
            for i in 0..8u32 {
                let expected = if i % 2 == 0 { None } else { Some(i) };
                assert_eq!(c.peek(&i), expected);
            }
            assert_eq!(c.metrics().evictions, Some(4));
            let expected_fires = if with_callback { 4 } else { 0 };
            assert_eq!(fired.load(Ordering::Relaxed), expected_fires);
        }
    }

    // --- cache_clear_with_on_evict: no-callback early return -----------------

    #[test]
    fn cache_clear_with_on_evict_without_callback_counts_across_shards() {
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(3600))
            .shards(8)
            .build()
            .unwrap();
        for i in 0..40u32 {
            SyncConcurrentCached::cache_set(&c, i, i).expect("insert must succeed");
        }
        c.cache_clear_with_on_evict();
        assert_eq!(c.len(), 0, "every shard must be emptied");
        assert!(c.is_empty());
        let per_shard = shard_eviction_counters(&c);
        assert_eq!(
            per_shard.iter().sum::<u64>(),
            40,
            "the no-callback path must still count every removed entry: {per_shard:?}"
        );
        assert!(
            per_shard.iter().filter(|&&n| n > 0).count() >= 2,
            "40 keys over 8 shards must move more than one counter: {per_shard:?}"
        );
        assert_eq!(c.metrics().evictions, Some(40));

        // Clearing an already-empty cache counts nothing.
        c.cache_clear_with_on_evict();
        assert_eq!(
            c.metrics().evictions,
            Some(40),
            "clearing an empty cache must not count evictions"
        );
    }

    // --- Expiry boundary under the single-sample clock -----------------------

    #[test]
    fn cache_get_evicts_at_the_expires_at_equals_now_boundary() {
        // `cache_get` samples the clock once, before taking the shard lock. `Instant` is
        // monotonic, so that sample is always >= the `expires_at` pinned here and the entry
        // must be judged expired (`now >= expires_at`), removed, and counted exactly once
        // on its own shard -- the same boundary `evict`/`retain` use.
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(3600))
            .shards(4)
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1, 10).expect("insert must succeed");
        SyncConcurrentCached::cache_set(&c, 2, 20).expect("insert must succeed");
        {
            let shard = c.shard_of(&1);
            let mut guard = shard.lock.write();
            guard.get_mut(&1).expect("key 1 stored").expires_at = Some(Instant::now());
        }
        assert_eq!(
            SyncConcurrentCached::cache_get(&c, &1).unwrap(),
            None,
            "an entry whose expires_at equals the sampled now must read as expired"
        );
        assert_eq!(
            SyncConcurrentCached::cache_get(&c, &2).unwrap(),
            Some(20),
            "an unexpired entry must be unaffected"
        );
        let mut expected = vec![0u64; 4];
        expected[owning_shard(&c, &1)] += 1;
        assert_eq!(shard_eviction_counters(&c), expected);
        assert_eq!(c.metrics().evictions, Some(1));
    }

    #[test]
    fn refresh_on_hit_get_evicts_at_the_boundary_and_renews_live_entries() {
        // Same boundary on the refresh_on_hit (write-lock) path, which now shares one clock
        // sample between the expiry check and the renewed expires_at stamp.
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(3600))
            .refresh_on_hit(true)
            .shards(4)
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1, 10).expect("insert must succeed");
        SyncConcurrentCached::cache_set(&c, 2, 20).expect("insert must succeed");
        {
            let shard = c.shard_of(&1);
            let mut guard = shard.lock.write();
            guard.get_mut(&1).expect("key 1 stored").expires_at = Some(Instant::now());
        }
        assert_eq!(
            SyncConcurrentCached::cache_get(&c, &1).unwrap(),
            None,
            "refresh_on_hit must apply the same now >= expires_at boundary"
        );
        assert_eq!(c.metrics().evictions, Some(1));

        // The live entry is renewed: its expires_at moves forward on the hit.
        let before = {
            let shard = c.shard_of(&2);
            let guard = shard.lock.read();
            guard.get(&2).expect("key 2 stored").expires_at
        };
        assert_eq!(SyncConcurrentCached::cache_get(&c, &2).unwrap(), Some(20));
        let after = {
            let shard = c.shard_of(&2);
            let guard = shard.lock.read();
            guard.get(&2).expect("key 2 stored").expires_at
        };
        assert!(
            after > before,
            "a refresh_on_hit hit must push expires_at forward (before={before:?}, after={after:?})"
        );
        assert_eq!(
            c.metrics().evictions,
            Some(1),
            "a renewed hit must not count an eviction"
        );
    }

    #[test]
    fn cache_get_with_expiry_status_uses_the_same_now_boundary() {
        for refresh in [false, true] {
            let c = ShardedTtlCache::<u32, u32>::builder()
                .ttl(Duration::from_secs(3600))
                .refresh_on_hit(refresh)
                .shards(1)
                .build()
                .unwrap();
            SyncConcurrentCached::cache_set(&c, 1, 10).expect("insert must succeed");
            {
                let shard = c.shard_of(&1);
                let mut guard = shard.lock.write();
                guard.get_mut(&1).expect("key 1 stored").expires_at = Some(Instant::now());
            }
            assert_eq!(
                ConcurrentCloneCached::cache_get_with_expiry_status(&c, &1),
                (Some(10), true),
                "expires_at == the sampled now must report expired (refresh={refresh})"
            );
            assert_eq!(
                c.metrics().evictions,
                Some(0),
                "the expiry-status read never evicts"
            );
            assert_eq!(c.len(), 1, "the expiry-status read never removes");
        }
    }

    // --- Certification: adversarial coverage of the once-per-call clock rewrite ------
    //
    // The rewrite has to be behavior-preserving on three axes: the two `evict` branches
    // (`extract_if` with a callback vs `retain` + length delta without one) must agree
    // exactly; a single clock sample per call must not shift any expiry decision; and the
    // per-shard eviction counters must behave under `deep_clone` / `cache_reset_metrics` /
    // `copy_from`. These tests approach those from the outside.

    /// Deterministic shard placement. `DefaultShardHasher` is randomly seeded per instance,
    /// so two independently built caches would scatter the same keys onto different shards
    /// and a shard-for-shard comparison between them would be meaningless.
    #[derive(Clone)]
    struct FixedShardHasher;

    impl ShardHasher<u32> for FixedShardHasher {
        fn shard_hash(&self, key: &u32) -> u64 {
            u64::from(*key).wrapping_mul(0x9e37_79b9_7f4a_7c15)
        }
    }

    /// Every stored entry as `(key, value, has_expiry)`, sorted. The `expires_at` instants
    /// of two independently built caches differ, so only the `Some`/`None` shape of the
    /// expiry is comparable across caches.
    fn entry_snapshot<H: ShardHasher<u32>>(
        c: &ShardedTtlCache<u32, u32, H>,
    ) -> Vec<(u32, u32, bool)> {
        let mut out: Vec<(u32, u32, bool)> = Vec::new();
        for shard in c.inner.shards.iter() {
            let guard = shard.lock.read();
            for (k, e) in guard.iter() {
                out.push((*k, e.value, e.expires_at.is_some()));
            }
        }
        out.sort_unstable();
        out
    }

    /// Force a stored entry's `expires_at`, bypassing the TTL stamping.
    fn set_expiry<H: ShardHasher<u32>>(
        c: &ShardedTtlCache<u32, u32, H>,
        k: u32,
        expires_at: Option<Instant>,
    ) {
        let shard = c.shard_of(&k);
        let mut guard = shard.lock.write();
        guard.get_mut(&k).expect("key stored").expires_at = expires_at;
    }

    /// `None` when the key is absent, `Some(expires_at)` (itself optional) when stored.
    fn stored_expiry<H: ShardHasher<u32>>(
        c: &ShardedTtlCache<u32, u32, H>,
        k: u32,
    ) -> Option<Option<Instant>> {
        let shard = c.shard_of(&k);
        let guard = shard.lock.read();
        guard.get(&k).map(|e| e.expires_at)
    }

    /// Keys `0..8` expired, `8..16` live, `16..24` never-expires (`expires_at == None`,
    /// what `unset_ttl` stamps) -- all three kinds mixed into the same shards.
    fn populate_mixed<H: ShardHasher<u32>>(c: &ShardedTtlCache<u32, u32, H>) {
        for i in 0..24u32 {
            SyncConcurrentCached::cache_set(c, i, i * 10).expect("insert must succeed");
        }
        let now = Instant::now();
        for i in 0..8u32 {
            set_expiry(c, i, Some(now));
        }
        for i in 16..24u32 {
            set_expiry(c, i, None);
        }
    }

    #[test]
    fn evict_callback_and_no_callback_branches_agree_exactly() {
        // Same input, both branches: identical return count, identical surviving state,
        // identical per-shard counters. The no-callback branch derives its count from
        // `before - guard.len()` rather than from the removed entries, so the two are
        // independent implementations of the same contract.
        let fired: Arc<std::sync::Mutex<Vec<(u32, u32)>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let fired2 = fired.clone();
        let with_cb = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(3600))
            .shards(4)
            .hasher(FixedShardHasher)
            .on_evict(move |k: &u32, v: &u32| {
                fired2.lock().expect("callback lock").push((*k, *v));
            })
            .build()
            .unwrap();
        let no_cb = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(3600))
            .shards(4)
            .hasher(FixedShardHasher)
            .build()
            .unwrap();
        populate_mixed(&with_cb);
        populate_mixed(&no_cb);
        assert_eq!(
            entry_snapshot(&with_cb),
            entry_snapshot(&no_cb),
            "the two caches must start from identical state"
        );

        let removed_cb = with_cb.evict();
        let removed_plain = no_cb.evict();
        assert_eq!(
            removed_cb, 8,
            "only the eight backdated entries are expired"
        );
        assert_eq!(
            removed_plain, removed_cb,
            "the retain + length-delta branch must return the same count as the extract_if branch"
        );
        assert_eq!(
            entry_snapshot(&with_cb),
            entry_snapshot(&no_cb),
            "both branches must leave identical state"
        );
        assert_eq!(
            shard_eviction_counters(&with_cb),
            shard_eviction_counters(&no_cb),
            "both branches must move the same per-shard counters"
        );
        let survivors: Vec<u32> = entry_snapshot(&no_cb)
            .into_iter()
            .map(|(k, ..)| k)
            .collect();
        assert_eq!(
            survivors,
            (8..24u32).collect::<Vec<u32>>(),
            "live and never-expires entries must survive both branches"
        );
        assert_eq!(
            entry_snapshot(&no_cb)
                .iter()
                .filter(|(.., has_expiry)| !has_expiry)
                .count(),
            8,
            "the never-expires entries must keep expires_at = None"
        );
        let mut fired_keys = fired.lock().expect("callback lock").clone();
        fired_keys.sort_unstable();
        assert_eq!(
            fired_keys,
            (0..8u32).map(|i| (i, i * 10)).collect::<Vec<_>>(),
            "on_evict must fire once per expired entry, and never for a never-expires one"
        );

        // A second sweep removes nothing: the zero-removal case of both branches.
        let counters = shard_eviction_counters(&with_cb);
        assert_eq!(with_cb.evict(), 0, "nothing is left to expire");
        assert_eq!(no_cb.evict(), 0, "nothing is left to expire");
        assert_eq!(
            shard_eviction_counters(&with_cb),
            counters,
            "a sweep that removes nothing must not touch any counter"
        );
        assert_eq!(shard_eviction_counters(&no_cb), counters);
        assert_eq!(
            fired.lock().expect("callback lock").len(),
            8,
            "a no-op sweep must not fire on_evict"
        );
    }

    #[test]
    fn evict_no_callback_length_delta_is_exact_for_all_and_none_expired_shards() {
        // The `before - guard.len()` arithmetic at its two extremes within one call: one
        // shard loses every entry, the other loses none.
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(3600))
            .shards(2)
            .hasher(FixedShardHasher)
            .build()
            .unwrap();
        let mut buckets: Vec<Vec<u32>> = vec![Vec::new(); 2];
        for k in 0..256u32 {
            let idx = owning_shard(&c, &k);
            if buckets[idx].len() < 4 {
                buckets[idx].push(k);
            }
        }
        assert!(
            buckets.iter().all(|b| b.len() == 4),
            "both shards must receive keys: {buckets:?}"
        );
        for k in buckets.iter().flatten() {
            SyncConcurrentCached::cache_set(&c, *k, *k * 10).expect("insert must succeed");
        }
        let now = Instant::now();
        for k in &buckets[0] {
            set_expiry(&c, *k, Some(now));
        }

        assert_eq!(c.evict(), 4, "only shard 0's entries are expired");
        assert_eq!(
            shard_eviction_counters(&c),
            vec![4, 0],
            "the emptied shard counts four, the untouched shard counts nothing"
        );
        assert_eq!(
            c.shard_sizes(),
            vec![0, 4],
            "the all-expired shard must be emptied and the none-expired shard untouched"
        );
        for k in &buckets[0] {
            assert_eq!(c.peek(k), None, "expired entries must be physically gone");
        }
        for k in &buckets[1] {
            assert_eq!(c.peek(k), Some(*k * 10), "live entries must be untouched");
        }
    }

    #[test]
    fn evict_never_expires_entries_survive_both_branches_in_one_shard() {
        // All three entry kinds in a *single* shard, so one `retain`/`extract_if` pass has
        // to discriminate them: `expires_at == None` is never swept, even with the TTL
        // subsequently disabled.
        for with_callback in [false, true] {
            let fired = Arc::new(AtomicU64::new(0));
            let fired2 = fired.clone();
            let mut builder = ShardedTtlCache::<u32, u32>::builder()
                .ttl(Duration::from_secs(3600))
                .shards(1);
            if with_callback {
                builder = builder.on_evict(move |_k: &u32, _v: &u32| {
                    fired2.fetch_add(1, Ordering::Relaxed);
                });
            }
            let c = builder.build().unwrap();
            for i in 0..8u32 {
                SyncConcurrentCached::cache_set(&c, i, i * 10).expect("insert must succeed");
            }
            let now = Instant::now();
            for i in 0..3u32 {
                set_expiry(&c, i, Some(now));
            }
            for i in 5..8u32 {
                set_expiry(&c, i, None);
            }

            assert_eq!(c.evict(), 3, "(with_callback={with_callback})");
            assert_eq!(c.len(), 5, "(with_callback={with_callback})");
            for i in 0..3u32 {
                assert_eq!(c.peek(&i), None);
            }
            for i in 3..8u32 {
                assert_eq!(c.peek(&i), Some(i * 10));
            }
            assert_eq!(c.metrics().evictions, Some(3));
            assert_eq!(
                fired.load(Ordering::Relaxed),
                if with_callback { 3 } else { 0 }
            );

            // Disabling the TTL must not make the never-expires entries sweepable, and must
            // not resurrect anything either.
            c.unset_ttl();
            assert_eq!(c.evict(), 0, "(with_callback={with_callback})");
            assert_eq!(c.len(), 5);
            assert_eq!(c.metrics().evictions, Some(3));
        }
    }

    #[test]
    fn refresh_on_hit_cache_get_trichotomy_absent_expired_live() {
        // The refresh path funnels through an `Option<Option<V>>` outcome: `None` (absent),
        // `Some(None)` (present but expired), `Some(Some(v))` (live, already refreshed).
        // Each arm has its own counter/callback/removal contract.
        let seen: Arc<std::sync::Mutex<Vec<(u32, u32)>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(3600))
            .refresh_on_hit(true)
            .shards(1)
            .on_evict(move |k: &u32, v: &u32| {
                seen2.lock().expect("callback lock").push((*k, *v));
            })
            .build()
            .unwrap();

        // 1) Absent: a miss, nothing else.
        assert_eq!(SyncConcurrentCached::cache_get(&c, &1).unwrap(), None);
        let m = c.metrics();
        assert_eq!((m.hits, m.misses, m.evictions), (Some(0), Some(1), Some(0)));
        assert!(seen.lock().expect("callback lock").is_empty());

        // 2) Live: a hit, the entry stays, and its expiry is pushed forward.
        SyncConcurrentCached::cache_set(&c, 2, 20).expect("insert must succeed");
        let before = stored_expiry(&c, 2).expect("key 2 stored");
        assert_eq!(SyncConcurrentCached::cache_get(&c, &2).unwrap(), Some(20));
        let after = stored_expiry(&c, 2).expect("key 2 stored");
        assert!(
            after > before,
            "a live refresh hit must renew expires_at (before={before:?}, after={after:?})"
        );
        let m = c.metrics();
        assert_eq!((m.hits, m.misses, m.evictions), (Some(1), Some(1), Some(0)));
        assert!(
            seen.lock().expect("callback lock").is_empty(),
            "a live hit must not fire on_evict"
        );

        // 3) Present but expired: a miss AND an eviction, entry physically removed.
        SyncConcurrentCached::cache_set(&c, 3, 30).expect("insert must succeed");
        set_expiry(&c, 3, Some(Instant::now()));
        assert_eq!(SyncConcurrentCached::cache_get(&c, &3).unwrap(), None);
        let m = c.metrics();
        assert_eq!(
            (m.hits, m.misses, m.evictions),
            (Some(1), Some(2), Some(1)),
            "an expired refresh read counts one miss and one eviction, no hit"
        );
        assert_eq!(
            *seen.lock().expect("callback lock"),
            vec![(3, 30)],
            "on_evict must fire once with the stored key and value"
        );
        assert_eq!(stored_expiry(&c, 3), None, "the entry must be removed");
        assert_eq!(c.len(), 1, "only the live entry remains");

        // 4) The now-absent key falls into the first arm: a plain miss, no second eviction.
        assert_eq!(SyncConcurrentCached::cache_get(&c, &3).unwrap(), None);
        let m = c.metrics();
        assert_eq!((m.hits, m.misses, m.evictions), (Some(1), Some(3), Some(1)));
        assert_eq!(seen.lock().expect("callback lock").len(), 1);
    }

    #[test]
    fn refresh_on_hit_with_ttl_disabled_keeps_each_entry_expiry_unchanged() {
        // `entry.expires_at = compute_expires_at(now).or(entry.expires_at)`: with the TTL
        // disabled, `compute_expires_at` is `None`, so a refresh hit must leave the stored
        // expiry exactly as it was -- it must neither clear it (making the entry immortal)
        // nor extend it.
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(3600))
            .refresh_on_hit(true)
            .shards(1)
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1, 10).expect("insert must succeed");
        let stamped = stored_expiry(&c, 1).expect("key 1 stored");
        assert!(stamped.is_some(), "the entry was stamped with the live TTL");

        assert_eq!(c.unset_ttl(), Some(Duration::from_secs(3600)));
        SyncConcurrentCached::cache_set(&c, 2, 20).expect("insert must succeed");
        assert_eq!(
            stored_expiry(&c, 2),
            Some(None),
            "an entry inserted with the TTL disabled never expires"
        );

        assert_eq!(SyncConcurrentCached::cache_get(&c, &1).unwrap(), Some(10));
        assert_eq!(
            stored_expiry(&c, 1),
            Some(stamped),
            "with the TTL disabled a refresh hit must leave the stored expiry untouched"
        );
        assert_eq!(SyncConcurrentCached::cache_get(&c, &2).unwrap(), Some(20));
        assert_eq!(
            stored_expiry(&c, 2),
            Some(None),
            "a never-expires entry must stay never-expires across a refresh hit"
        );

        // The retained stamp still governs: the entry expires on its own schedule even
        // though the TTL is disabled cache-wide.
        set_expiry(&c, 1, Some(Instant::now()));
        assert_eq!(
            SyncConcurrentCached::cache_get(&c, &1).unwrap(),
            None,
            "disabling the TTL must not resurrect an already-stamped entry"
        );
        assert_eq!(c.metrics().evictions, Some(1));
    }

    #[test]
    fn cache_set_judges_the_displaced_entry_against_the_stamping_now() {
        // `cache_set` samples the clock once and uses that instant both to stamp the new
        // entry and to judge the displaced one.
        let fired = Arc::new(AtomicU64::new(0));
        let fired2 = fired.clone();
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(3600))
            .shards(1)
            .on_evict(move |_k: &u32, _v: &u32| {
                fired2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1, 10).expect("insert must succeed");

        // A live displaced entry is returned, uncounted.
        assert_eq!(
            SyncConcurrentCached::cache_set(&c, 1, 11).unwrap(),
            Some(10)
        );
        assert_eq!(c.metrics().evictions, Some(0));
        assert_eq!(fired.load(Ordering::Relaxed), 0);

        // At the `now >= expires_at` boundary the displaced entry is expired: filtered from
        // the return, counted, and passed to on_evict.
        set_expiry(&c, 1, Some(Instant::now()));
        assert_eq!(
            SyncConcurrentCached::cache_set(&c, 1, 12).unwrap(),
            None,
            "an entry whose expires_at equals the sampled now must count as displaced-expired"
        );
        assert_eq!(c.metrics().evictions, Some(1));
        assert_eq!(fired.load(Ordering::Relaxed), 1);
        assert_eq!(
            SyncConcurrentCached::cache_get(&c, &1).unwrap(),
            Some(12),
            "the replacement is stamped live against the same instant"
        );

        // A displaced never-expires entry is never treated as expired.
        set_expiry(&c, 1, None);
        assert_eq!(
            SyncConcurrentCached::cache_set(&c, 1, 13).unwrap(),
            Some(12)
        );
        assert_eq!(c.metrics().evictions, Some(1));
        assert_eq!(fired.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn cache_set_with_ttl_disabled_still_evicts_the_displaced_expired_entry() {
        // `unset_ttl` between the insert and the overwrite: the replacement is stamped
        // never-expires, but the displaced entry keeps its own stamp and is still judged
        // against the same `now`.
        let fired = Arc::new(AtomicU64::new(0));
        let fired2 = fired.clone();
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(3600))
            .shards(1)
            .on_evict(move |_k: &u32, _v: &u32| {
                fired2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1, 10).expect("insert must succeed");
        set_expiry(&c, 1, Some(Instant::now()));
        c.unset_ttl();

        assert_eq!(
            SyncConcurrentCached::cache_set(&c, 1, 20).unwrap(),
            None,
            "the displaced entry was expired when the replacement was stamped"
        );
        assert_eq!(c.metrics().evictions, Some(1));
        assert_eq!(fired.load(Ordering::Relaxed), 1);
        assert_eq!(
            stored_expiry(&c, 1),
            Some(None),
            "the replacement must be stamped never-expires"
        );
        assert_eq!(c.evict(), 0, "a never-expires entry is never swept");
        assert_eq!(SyncConcurrentCached::cache_get(&c, &1).unwrap(), Some(20));
    }

    #[test]
    fn cache_set_judges_the_displaced_entry_against_its_pre_lock_sample() {
        // `cache_set` samples the clock *before* it acquires the shard write lock, so an
        // entry that crosses its expiry while the caller is queued on that lock is still
        // judged live: it is handed back to the caller and no eviction is counted. This is
        // the direct consequence of sampling once per call -- a version that re-read the
        // clock after taking the lock would report the same displacement as expired
        // (`None` + one eviction + an `on_evict` fire). The window is opened here with a
        // `retain` predicate, which runs while the shard write guard is held.
        const HOLD_MS: u64 = 500;
        const EXPIRE_IN_MS: u64 = 200;
        const START_DELAY_MS: u64 = 50;

        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(3600))
            .shards(1)
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1, 10).expect("insert must succeed");
        let expires_at = Instant::now() + Duration::from_millis(EXPIRE_IN_MS);
        set_expiry(&c, 1, Some(expires_at));

        let gate = Arc::new(std::sync::Barrier::new(2));
        let holder = {
            let c = c.clone();
            let gate = gate.clone();
            std::thread::spawn(move || {
                gate.wait();
                // The entry is live when `retain` samples its own instant, so it is kept --
                // the predicate only holds the shard's write lock.
                c.retain(|_k, _v| {
                    std::thread::sleep(std::time::Duration::from_millis(HOLD_MS));
                    true
                });
            })
        };
        let setter = {
            let c = c.clone();
            let gate = gate.clone();
            std::thread::spawn(move || {
                gate.wait();
                std::thread::sleep(std::time::Duration::from_millis(START_DELAY_MS));
                let sampled = Instant::now();
                let displaced = SyncConcurrentCached::cache_set(&c, 1, 20).unwrap();
                (sampled, Instant::now(), displaced)
            })
        };
        holder.join().expect("holder thread must not panic");
        let (sampled, finished, displaced) = setter.join().expect("setter thread must not panic");

        assert!(
            sampled < expires_at,
            "test timing: the setter must sample the clock while the entry is still live"
        );
        assert!(
            finished > expires_at,
            "test timing: the setter must stay blocked on the shard lock past the expiry"
        );
        assert_eq!(
            displaced,
            Some(10),
            "the displaced entry is judged against the caller's own pre-lock sample, at \
             which it was still live"
        );
        assert_eq!(
            c.metrics().evictions,
            Some(0),
            "a displacement judged live must not count an eviction"
        );
        assert_eq!(
            SyncConcurrentCached::cache_get(&c, &1).unwrap(),
            Some(20),
            "the replacement is stamped with the current TTL and is live"
        );
    }

    #[test]
    fn changing_the_ttl_never_restamps_stored_entries() {
        // Runtime TTL changes apply to future stamps only. Entries carry their own
        // `expires_at`, so shrinking the TTL must not retroactively expire live entries and
        // growing it must not revive expired ones.
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(3600))
            .shards(1)
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1, 10).expect("insert must succeed");
        let stamped = stored_expiry(&c, 1).expect("key 1 stored");

        assert_eq!(
            c.set_ttl(Duration::from_millis(1)),
            Some(Duration::from_secs(3600))
        );
        SyncConcurrentCached::cache_set(&c, 2, 20).expect("insert must succeed");
        std::thread::sleep(std::time::Duration::from_millis(30));

        assert_eq!(
            stored_expiry(&c, 1),
            Some(stamped),
            "shrinking the TTL must not restamp a stored entry"
        );
        assert_eq!(
            SyncConcurrentCached::cache_get(&c, &1).unwrap(),
            Some(10),
            "the long-stamped entry is still live"
        );
        assert_eq!(
            SyncConcurrentCached::cache_get(&c, &2).unwrap(),
            None,
            "the short-stamped entry expired on its own stamp"
        );
        assert_eq!(c.metrics().evictions, Some(1));

        // Growing the TTL back does not revive anything already removed, nor un-expire an
        // entry whose stamp has passed.
        SyncConcurrentCached::cache_set(&c, 3, 30).expect("insert must succeed");
        std::thread::sleep(std::time::Duration::from_millis(30));
        c.set_ttl(Duration::from_secs(3600));
        assert_eq!(
            SyncConcurrentCached::cache_get(&c, &3).unwrap(),
            None,
            "growing the TTL must not un-expire an already-stamped entry"
        );
        assert_eq!(c.metrics().evictions, Some(2));
        assert_eq!(SyncConcurrentCached::cache_get(&c, &1).unwrap(), Some(10));
    }

    #[test]
    fn deep_clone_counters_are_independent_of_reset_metrics_on_either_handle() {
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(3600))
            .shards(4)
            .hasher(FixedShardHasher)
            .build()
            .unwrap();
        for i in 0..16u32 {
            SyncConcurrentCached::cache_set(&c, i, i * 10).expect("insert must succeed");
        }
        // Earn one of each counter kind.
        assert_eq!(SyncConcurrentCached::cache_get(&c, &0).unwrap(), Some(0));
        assert_eq!(SyncConcurrentCached::cache_get(&c, &999).unwrap(), None);
        assert_eq!(
            SyncConcurrentCached::cache_remove(&c, &1).unwrap(),
            Some(10)
        );
        let source_counters = shard_eviction_counters(&c);
        let source_metrics = c.metrics();
        assert_eq!(
            (
                source_metrics.hits,
                source_metrics.misses,
                source_metrics.evictions
            ),
            (Some(1), Some(1), Some(1))
        );

        let d = c.deep_clone();
        assert_eq!(shard_eviction_counters(&d), source_counters);
        let clone_metrics = d.metrics();
        assert_eq!(
            (
                clone_metrics.hits,
                clone_metrics.misses,
                clone_metrics.evictions,
                clone_metrics.entry_count
            ),
            (
                source_metrics.hits,
                source_metrics.misses,
                source_metrics.evictions,
                source_metrics.entry_count
            ),
            "deep_clone must carry every counter over"
        );

        // Reset on the source: the clone's counters are untouched.
        ConcurrentCached::cache_reset_metrics(&c).unwrap();
        assert_eq!(c.metrics().evictions, Some(0));
        assert_eq!(shard_eviction_counters(&c), vec![0u64; 4]);
        assert_eq!(
            shard_eviction_counters(&d),
            source_counters,
            "resetting the source must not touch the clone"
        );
        assert_eq!(d.metrics().evictions, Some(1));
        assert_eq!(c.len(), 15, "cache_reset_metrics must not remove any entry");

        // Reset on the clone: the source (which has since earned counts again) is untouched.
        assert!(SyncConcurrentCached::cache_delete(&c, &2).unwrap());
        assert_eq!(c.metrics().evictions, Some(1));
        ConcurrentCached::cache_reset_metrics(&d).unwrap();
        assert_eq!(shard_eviction_counters(&d), vec![0u64; 4]);
        assert_eq!(d.metrics().evictions, Some(0));
        assert_eq!(
            c.metrics().evictions,
            Some(1),
            "resetting the clone must not touch the source"
        );

        // The entry maps are independent too.
        d.clear();
        assert_eq!(d.len(), 0);
        assert_eq!(c.len(), 14, "clearing the clone must not empty the source");
    }

    #[test]
    fn copy_from_carries_entries_but_no_counters_and_fires_no_callback() {
        // `copy_from` builds a fresh cache: it starts with zeroed counters even though the
        // source has counts, and it reads (never removes) from the source, so the source's
        // on_evict must stay silent.
        let fired = Arc::new(AtomicU64::new(0));
        let fired2 = fired.clone();
        let src = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(3600))
            .shards(4)
            .on_evict(move |_k: &u32, _v: &u32| {
                fired2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();
        for i in 0..16u32 {
            SyncConcurrentCached::cache_set(&src, i, i * 10).expect("insert must succeed");
        }
        assert_eq!(SyncConcurrentCached::cache_get(&src, &0).unwrap(), Some(0));
        assert_eq!(SyncConcurrentCached::cache_get(&src, &999).unwrap(), None);
        assert_eq!(
            SyncConcurrentCached::cache_remove(&src, &1).unwrap(),
            Some(10)
        );
        // A never-expires entry (TTL disabled at insert time) must survive the copy.
        src.unset_ttl();
        SyncConcurrentCached::cache_set(&src, 100, 1000).expect("insert must succeed");
        src.set_ttl(Duration::from_secs(3600));
        // An expired entry must not.
        SyncConcurrentCached::cache_set(&src, 200, 2000).expect("insert must succeed");
        set_expiry(&src, 200, Some(Instant::now()));

        let src_before = src.metrics();
        let fired_before = fired.load(Ordering::Relaxed);
        assert_eq!(
            fired_before, 1,
            "only the explicit remove fired the callback"
        );

        let dst = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(3600))
            .shards(4)
            .copy_from(&src)
            .unwrap();

        let dst_metrics = dst.metrics();
        assert_eq!(
            (dst_metrics.hits, dst_metrics.misses, dst_metrics.evictions),
            (Some(0), Some(0), Some(0)),
            "copy_from carries no counters at all"
        );
        assert_eq!(shard_eviction_counters(&dst), vec![0u64; 4]);
        assert_eq!(
            dst.len(),
            16,
            "fifteen live originals plus the never-expires entry, minus the expired one"
        );
        for i in 2..16u32 {
            assert_eq!(dst.peek(&i), Some(i * 10));
        }
        assert_eq!(
            dst.peek(&1),
            None,
            "an entry removed before the copy is gone"
        );
        assert_eq!(dst.peek(&200), None, "an expired entry is skipped");
        assert_eq!(
            stored_expiry(&dst, 100),
            Some(None),
            "a never-expires entry keeps its None stamp through the copy"
        );

        assert_eq!(
            fired.load(Ordering::Relaxed),
            fired_before,
            "copy_from must not fire the source's on_evict"
        );
        let src_after = src.metrics();
        assert_eq!(
            (
                src_after.hits,
                src_after.misses,
                src_after.evictions,
                src_after.entry_count
            ),
            (
                src_before.hits,
                src_before.misses,
                src_before.evictions,
                src_before.entry_count
            ),
            "copy_from must leave the source's counters and entries alone"
        );
    }

    #[test]
    fn metrics_and_cache_evictions_agree_after_concurrent_evictions() {
        // Per-shard relaxed loads mean a *mid-flight* aggregate can be torn, so only two
        // things are guaranteed and asserted here: (1) an observer's successive aggregates
        // never go backwards and never exceed the true total, because every per-shard read
        // of one snapshot happens after every read of the previous one; (2) once the writers
        // are joined, `metrics()`, `cache_evictions()` and the raw per-shard sum agree
        // exactly.
        const THREADS: u32 = 8;
        const OPS: u32 = 250;
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(3600))
            .shards(8)
            .build()
            .unwrap();
        let total = u64::from(THREADS * OPS);

        let stop = Arc::new(AtomicBool::new(false));
        let observer = {
            let c = c.clone();
            let stop = stop.clone();
            std::thread::spawn(move || {
                let mut last = 0u64;
                let mut samples = 0u64;
                while !stop.load(Ordering::Relaxed) {
                    let seen = c.metrics().evictions.expect("evictions tracked");
                    assert!(
                        seen >= last,
                        "a later aggregate must not go backwards ({seen} < {last})"
                    );
                    assert!(
                        seen <= total,
                        "the aggregate must never exceed the true total"
                    );
                    last = seen;
                    samples += 1;
                }
                samples
            })
        };

        let mut handles = Vec::new();
        for t in 0..THREADS {
            let c = c.clone();
            handles.push(std::thread::spawn(move || {
                for i in 0..OPS {
                    let k = t * OPS + i;
                    SyncConcurrentCached::cache_set(&c, k, k).expect("insert must succeed");
                    assert_eq!(
                        SyncConcurrentCached::cache_remove(&c, &k).unwrap(),
                        Some(k),
                        "each thread owns a disjoint key range"
                    );
                }
            }));
        }
        for h in handles {
            h.join().expect("worker thread must not panic");
        }
        stop.store(true, Ordering::Relaxed);
        let samples = observer.join().expect("observer thread must not panic");
        assert!(
            samples > 0,
            "the observer must have taken at least one sample"
        );

        assert_eq!(c.len(), 0, "every key was removed again");
        assert_eq!(
            c.metrics().evictions,
            Some(total),
            "every removal must be counted exactly once"
        );
        assert_eq!(ConcurrentCacheBase::cache_evictions(&c), Some(total));
        assert_eq!(
            shard_eviction_counters(&c).iter().sum::<u64>(),
            total,
            "the raw per-shard counters must sum to the same total"
        );
    }

    #[test]
    fn peek_contains_and_remove_apply_the_same_now_boundary() {
        // The remaining converted read paths keep `now >= expires_at`, and the
        // non-removing ones stay non-removing.
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(3600))
            .shards(1)
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1, 10).expect("insert must succeed");
        SyncConcurrentCached::cache_set(&c, 2, 20).expect("insert must succeed");
        set_expiry(&c, 1, Some(Instant::now()));

        assert!(!c.contains(&1), "expires_at == now reads as expired");
        assert!(c.contains(&2));
        assert_eq!(c.peek(&1), None);
        assert_eq!(c.peek(&2), Some(20));
        assert_eq!(
            ConcurrentCloneCached::cache_peek_with_expiry_status(&c, &1),
            (Some(10), true)
        );
        assert_eq!(c.len(), 2, "peek and contains must not remove anything");
        assert_eq!(
            c.metrics().evictions,
            Some(0),
            "peek and contains must not count evictions"
        );

        assert_eq!(
            SyncConcurrentCached::cache_remove(&c, &1).unwrap(),
            None,
            "an expired entry is filtered from cache_remove's return"
        );
        assert_eq!(
            c.metrics().evictions,
            Some(1),
            "cache_remove still counts the removal of an expired entry"
        );
        assert_eq!(c.len(), 1);
        assert_eq!(
            SyncConcurrentCached::cache_remove_entry(&c, &2).unwrap(),
            Some((2, 20))
        );
        assert_eq!(c.metrics().evictions, Some(2));
        assert_eq!(c.len(), 0);
    }

    #[test]
    fn evict_is_callable_through_both_entry_points_under_the_key_clone_bound() {
        // The one-pass rewrite no longer clones keys, but the public `K: Clone` bound on
        // the inherent `evict` and on the `ConcurrentCacheEvict` impl was deliberately kept.
        // A passing test cannot prove a bound is still *required* (that needs a
        // compile-fail harness), so this pins the callable surface: both entry points
        // resolve for a `K: Clone` key and agree on the swept count.
        fn evict_both_ways<K: Clone + Hash + Eq, V: Clone>(c: &ShardedTtlCache<K, V>) -> usize {
            let via_inherent = ShardedTtlCache::evict(c);
            let via_trait = ConcurrentCacheEvict::evict(c);
            via_inherent + via_trait
        }
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(3600))
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1, 10).expect("insert must succeed");
        assert_eq!(evict_both_ways(&c), 0, "nothing is expired");
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn peek_expires_at_absent_key_returns_none_none() {
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(3600))
            .build()
            .unwrap();
        assert_eq!(
            ConcurrentCacheExpiry::cache_peek_expires_at(&c, &1),
            (None, None)
        );
        assert_eq!(ConcurrentCacheExpiry::peek_expires_at(&c, &1), (None, None));
    }

    #[test]
    fn peek_expires_at_live_entry_returns_the_stored_future_deadline() {
        let ttl = Duration::from_secs(3600);
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(ttl)
            .build()
            .unwrap();
        let before = Instant::now();
        SyncConcurrentCached::cache_set(&c, 1, 10).expect("insert must succeed");
        let after = Instant::now();

        let stored = stored_expiry(&c, 1)
            .expect("entry must be present")
            .expect("a configured ttl must record a deadline");

        let (value, expires_at) = ConcurrentCacheExpiry::cache_peek_expires_at(&c, &1);
        assert_eq!(value, Some(10));
        assert_eq!(
            expires_at,
            Some(stored),
            "the reported deadline must be the one the shard holds"
        );
        let expires_at = expires_at.unwrap();
        assert!(expires_at > Instant::now(), "a live entry expires later");
        assert!(expires_at >= before + ttl && expires_at <= after + ttl);
    }

    #[test]
    fn peek_expires_at_never_expiring_entry_reports_no_deadline() {
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(3600))
            .build()
            .unwrap();
        // Disabling the ttl stores entries without a deadline.
        c.unset_ttl();
        SyncConcurrentCached::cache_set(&c, 1, 10).expect("insert must succeed");
        assert_eq!(
            ConcurrentCacheExpiry::cache_peek_expires_at(&c, &1),
            (Some(10), None)
        );
        // Distinguishable from an absent key by the value, not by the deadline.
        assert_eq!(
            ConcurrentCloneCached::cache_peek_with_expiry_status(&c, &1),
            (Some(10), false)
        );
    }

    #[test]
    fn peek_expires_at_expired_entry_returns_a_past_deadline_and_keeps_the_entry() {
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(3600))
            .shards(1)
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1, 10).expect("insert must succeed");
        let deadline = Instant::now();
        set_expiry(&c, 1, Some(deadline));

        let (value, expires_at) = ConcurrentCacheExpiry::cache_peek_expires_at(&c, &1);
        assert_eq!(value, Some(10), "an expired entry is still returned");
        assert_eq!(expires_at, Some(deadline), "the past deadline is reported");
        assert!(expires_at.unwrap() <= Instant::now());
        // Not removed by the peek, and no eviction counted.
        assert_eq!(c.len(), 1);
        assert_eq!(c.metrics().evictions, Some(0));
        assert_eq!(
            ConcurrentCacheExpiry::cache_peek_expires_at(&c, &1),
            (Some(10), Some(deadline))
        );
    }

    #[test]
    fn peek_expires_at_deadline_is_past_exactly_when_peek_reports_expired() {
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(3600))
            .shards(1)
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1, 10).expect("insert must succeed");
        for expire in [false, true] {
            if expire {
                set_expiry(&c, 1, Some(Instant::now()));
            }
            let (_, expires_at) = ConcurrentCacheExpiry::cache_peek_expires_at(&c, &1);
            let (_, expired) = ConcurrentCloneCached::cache_peek_with_expiry_status(&c, &1);
            assert_eq!(
                expires_at.is_some_and(|t| t <= Instant::now()),
                expired,
                "the deadline must be in the past exactly when the peek reports expired"
            );
        }
    }

    #[test]
    fn peek_expires_at_does_not_touch_hit_or_miss_counters() {
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(3600))
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1, 10).expect("insert must succeed");
        let hits = c.metrics().hits;
        let misses = c.metrics().misses;

        let _ = ConcurrentCacheExpiry::cache_peek_expires_at(&c, &1); // present
        let _ = ConcurrentCacheExpiry::cache_peek_expires_at(&c, &2); // absent

        assert_eq!(c.metrics().hits, hits, "a peek must not count a hit");
        assert_eq!(c.metrics().misses, misses, "a peek must not count a miss");
    }

    #[test]
    fn peek_expires_at_does_not_renew_the_ttl_with_refresh_on_hit() {
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_millis(200))
            .build()
            .unwrap();
        c.set_refresh_on_hit(true);
        SyncConcurrentCached::cache_set(&c, 1, 10).expect("insert must succeed");

        let (_, first) = ConcurrentCacheExpiry::cache_peek_expires_at(&c, &1);
        std::thread::sleep(std::time::Duration::from_millis(40));
        let (_, second) = ConcurrentCacheExpiry::cache_peek_expires_at(&c, &1);
        assert_eq!(
            first, second,
            "peeking must not renew the ttl even with refresh_on_hit enabled"
        );

        // Control: a real hit does renew, so the assertion above is not vacuous.
        assert_eq!(SyncConcurrentCached::cache_get(&c, &1).unwrap(), Some(10));
        let (_, after_hit) = ConcurrentCacheExpiry::cache_peek_expires_at(&c, &1);
        assert!(
            after_hit > first,
            "refresh_on_hit must extend the deadline on a real read"
        );
    }

    // Gap 1: unlike the single-owner TtlCache (whose Duration ttl is stored unclamped, so
    // Duration::MAX reliably overflows Instant::checked_add and yields expires_at = None --
    // see cache_set_with_ttl_overflow_stores_never_expiring_entry in stores/ttl.rs), the
    // sharded store's ttl_nanos atomic clamps to u64::MAX nanoseconds (~584 years) before
    // compute_expires_at ever runs, so a Duration::MAX ttl does NOT reach the overflow branch
    // in practice here. Pin the actual observable behavior: peek_expires_at reports a real,
    // very distant deadline, not None.
    #[test]
    fn peek_expires_at_extreme_ttl_is_clamped_not_overflowed() {
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        c.set_ttl(Duration::MAX);
        let before = Instant::now();
        SyncConcurrentCached::cache_set(&c, 1, 42).expect("insert must succeed");
        let after = Instant::now();

        let (value, expires_at) = ConcurrentCacheExpiry::cache_peek_expires_at(&c, &1);
        assert_eq!(value, Some(42));
        let expires_at = expires_at.expect(
            "an extreme ttl is clamped to ~584 years, not overflowed to None, unlike TtlCache",
        );
        let clamped_ttl = Duration::from_nanos(u64::MAX);
        assert!(
            expires_at >= before + clamped_ttl && expires_at <= after + clamped_ttl,
            "the deadline must reflect the clamped ~584-year ttl, not an overflow-to-None"
        );
        assert_eq!(
            ConcurrentCacheExpiry::peek_expires_at(&c, &1),
            ConcurrentCacheExpiry::cache_peek_expires_at(&c, &1)
        );
    }

    // Gap 2: changing the store's ttl (including disabling it) must NOT retroactively touch a
    // deadline an already-stored entry carries -- set_ttl/unset_ttl only swap the shared
    // ttl_nanos atomic and never walk existing entries. peek_expires_at must keep reporting the
    // stale deadline.
    #[test]
    fn peek_expires_at_reports_stale_deadline_after_ttl_change() {
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1, 100).expect("insert must succeed");
        let (_, original) = ConcurrentCacheExpiry::cache_peek_expires_at(&c, &1);
        let original = original.expect("a configured ttl must record a deadline");

        // Shrinking the ttl must not touch the entry already stored.
        c.set_ttl(Duration::from_secs(5));
        assert_eq!(
            ConcurrentCacheExpiry::cache_peek_expires_at(&c, &1),
            (Some(100), Some(original)),
            "changing the store ttl must not retroactively rewrite an existing entry's deadline"
        );

        // Disabling the ttl entirely must not clear the deadline either -- only future
        // inserts/refreshes are affected.
        c.unset_ttl();
        assert_eq!(
            ConcurrentCacheExpiry::cache_peek_expires_at(&c, &1),
            (Some(100), Some(original)),
            "disabling the ttl must not clear an already-stored entry's deadline"
        );

        // A fresh insert after disabling the ttl, in contrast, has no deadline.
        SyncConcurrentCached::cache_set(&c, 2, 200).expect("insert must succeed");
        assert_eq!(
            ConcurrentCacheExpiry::cache_peek_expires_at(&c, &2),
            (Some(200), None)
        );
    }

    // Gap: the crate's documented convention is `now >= expires_at` means expired (see the
    // boundary test at src/stores/lru_ttl.rs:2775). Pin that peek_expires_at's raw deadline and
    // cache_peek_with_expiry_status's liveness judgement agree exactly at the tie,
    // deterministically (no sleep): `tie` is sampled before it is written into the entry, so by
    // the time it is read back, real "now" is guaranteed to be >= `tie`.
    #[test]
    fn peek_expires_at_boundary_matches_now_ge_expires_at_convention() {
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(3600))
            .shards(1)
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1, 100).expect("insert must succeed");
        let tie = Instant::now();
        set_expiry(&c, 1, Some(tie));

        assert_eq!(
            ConcurrentCacheExpiry::cache_peek_expires_at(&c, &1),
            (Some(100), Some(tie))
        );
        assert_eq!(
            ConcurrentCloneCached::cache_peek_with_expiry_status(&c, &1),
            (Some(100), true),
            "now == expires_at must be treated as expired, matching the now >= expires_at convention"
        );
    }

    // Gap: peek_expires_at must reflect physical removal -- both evict() and an explicit
    // cache_remove -- by reporting the absent-key shape afterward.
    #[test]
    fn peek_expires_at_reports_absent_after_evict_removes_the_entry() {
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_millis(20))
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1, 100).expect("insert must succeed");
        std::thread::sleep(std::time::Duration::from_millis(60));

        let (value, expires_at) = ConcurrentCacheExpiry::cache_peek_expires_at(&c, &1);
        assert_eq!(value, Some(100));
        assert!(expires_at.unwrap() <= Instant::now());

        assert_eq!(
            c.evict(),
            1,
            "evict must physically remove the expired entry"
        );
        assert_eq!(
            ConcurrentCacheExpiry::cache_peek_expires_at(&c, &1),
            (None, None),
            "a physically removed entry must be reported as absent"
        );
    }

    #[test]
    fn peek_expires_at_reports_absent_after_cache_remove() {
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1, 100).expect("insert must succeed");
        assert_eq!(
            SyncConcurrentCached::cache_remove(&c, &1).unwrap(),
            Some(100)
        );
        assert_eq!(
            ConcurrentCacheExpiry::cache_peek_expires_at(&c, &1),
            (None, None)
        );
        assert_eq!(ConcurrentCacheExpiry::peek_expires_at(&c, &1), (None, None));
    }

    // Gap 5: the ergonomic alias must agree with the canonical method across every return shape
    // the contract defines, not just the absent-key case the implementor already covered.
    #[test]
    fn peek_expires_at_alias_matches_canonical_across_all_return_shapes() {
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_millis(20))
            .build()
            .unwrap();

        // absent
        assert_eq!(
            ConcurrentCacheExpiry::peek_expires_at(&c, &1),
            ConcurrentCacheExpiry::cache_peek_expires_at(&c, &1)
        );
        assert_eq!(ConcurrentCacheExpiry::peek_expires_at(&c, &1), (None, None));

        // live
        SyncConcurrentCached::cache_set(&c, 1, 100).expect("insert must succeed");
        assert_eq!(
            ConcurrentCacheExpiry::peek_expires_at(&c, &1),
            ConcurrentCacheExpiry::cache_peek_expires_at(&c, &1)
        );
        assert!(ConcurrentCacheExpiry::peek_expires_at(&c, &1).1.unwrap() > Instant::now());

        // expired, not removed
        std::thread::sleep(std::time::Duration::from_millis(60));
        assert_eq!(
            ConcurrentCacheExpiry::peek_expires_at(&c, &1),
            ConcurrentCacheExpiry::cache_peek_expires_at(&c, &1)
        );
        assert!(ConcurrentCacheExpiry::peek_expires_at(&c, &1).1.unwrap() <= Instant::now());

        // never-expiring
        c.unset_ttl();
        SyncConcurrentCached::cache_set(&c, 2, 200).expect("insert must succeed");
        assert_eq!(
            ConcurrentCacheExpiry::peek_expires_at(&c, &2),
            (Some(200), None)
        );
        assert_eq!(
            ConcurrentCacheExpiry::peek_expires_at(&c, &2),
            ConcurrentCacheExpiry::cache_peek_expires_at(&c, &2)
        );
    }

    // Gap 4: nothing else in the suite calls ConcurrentCacheExpiry through a generic
    // `T: ConcurrentCacheExpiry<K, V>` bound or through `cached::prelude::*` -- both a
    // monomorphization/dyn-compat regression and a prelude export regression would go
    // uncaught otherwise.
    #[test]
    fn concurrent_cache_expiry_is_reachable_through_a_generic_bound_and_the_prelude() {
        // Mirrors an external `use cached::prelude::*;`, independent of the direct
        // `use crate::ConcurrentCacheExpiry` import at the top of this file.
        use crate::prelude::*;

        fn peek_via_bound<T: ConcurrentCacheExpiry<u32, u32>>(
            store: &T,
            key: &u32,
        ) -> (Option<u32>, Option<crate::time::Instant>) {
            store.cache_peek_expires_at(key)
        }

        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1, 100).expect("insert must succeed");

        let (value, expires_at) = peek_via_bound(&c, &1);
        assert_eq!(value, Some(100));
        assert!(expires_at.is_some());
        assert_eq!(
            peek_via_bound(&c, &2),
            (None, None),
            "absent key via the generic bound"
        );
    }

    // Gap 3a: existing peek_expires_at tests only ever exercise `shards(1)`. Route several
    // distinct keys through a multi-shard cache and confirm each is read back correctly --
    // including the mix of expired / live / never-expiring entries -- regardless of which
    // physical shard it landed in.
    #[test]
    fn peek_expires_at_routes_correctly_across_many_shards() {
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(3600))
            .shards(8)
            .build()
            .unwrap();
        populate_mixed(&c);

        // Sanity: the fixture must actually span multiple physical shards, otherwise this test
        // would not exercise cross-shard routing at all.
        let distinct_shards: std::collections::HashSet<usize> = (0..24u32)
            .map(|k| c.shard_of(&k) as *const _ as usize)
            .collect();
        assert!(
            distinct_shards.len() > 1,
            "fixture must span multiple shards for this test to be meaningful"
        );

        for i in 0..8u32 {
            let (value, expires_at) = ConcurrentCacheExpiry::cache_peek_expires_at(&c, &i);
            assert_eq!(value, Some(i * 10), "key {i} (expired group)");
            assert!(
                expires_at.unwrap() <= Instant::now(),
                "key {i} must carry a past deadline"
            );
        }
        for i in 8..16u32 {
            let (value, expires_at) = ConcurrentCacheExpiry::cache_peek_expires_at(&c, &i);
            assert_eq!(value, Some(i * 10), "key {i} (live group)");
            assert!(
                expires_at.unwrap() > Instant::now(),
                "key {i} must carry a future deadline"
            );
        }
        for i in 16..24u32 {
            let (value, expires_at) = ConcurrentCacheExpiry::cache_peek_expires_at(&c, &i);
            assert_eq!(value, Some(i * 10), "key {i} (never-expiring group)");
            assert_eq!(expires_at, None, "key {i} must carry no deadline");
        }
        assert_eq!(
            ConcurrentCacheExpiry::cache_peek_expires_at(&c, &999u32),
            (None, None),
            "an absent key must report absent regardless of shard count"
        );
    }

    // Gap 3b: a writer on one thread (through an Arc-shared clone) updates a key's deadline;
    // a subsequent peek on the original handle, after the writer has joined, must observe that
    // update -- exercising cross-thread visibility through the shard lock rather than the
    // single-threaded round-trips every other peek_expires_at test performs.
    #[test]
    fn peek_expires_at_observes_a_concurrent_writers_deadline_update() {
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(3600))
            .shards(8)
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1, 10).expect("insert must succeed");
        let (_, before) = ConcurrentCacheExpiry::cache_peek_expires_at(&c, &1);
        let before = before.expect("a configured ttl must record a deadline");

        // Arc-share clone: both handles refer to the same underlying shards.
        let writer = c.clone();
        let handle = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(30));
            // Overwriting a live entry stamps a fresh deadline anchored to the write time.
            SyncConcurrentCached::cache_set(&writer, 1, 20).expect("overwrite must succeed");
        });
        handle.join().expect("writer thread must not panic");

        let (value, after) = ConcurrentCacheExpiry::cache_peek_expires_at(&c, &1);
        assert_eq!(
            value,
            Some(20),
            "peek on the original handle must observe the writer thread's value"
        );
        assert!(
            after.expect("still ttl-bearing") > before,
            "peek must observe the writer thread's updated (later) deadline"
        );
    }

    // --- ConcurrentCacheExpiry::cache_expires_at (the value-free read) ---

    #[test]
    fn expires_at_absent_key_returns_false_none() {
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        assert_eq!(
            ConcurrentCacheExpiry::cache_expires_at(&c, &1),
            (false, None)
        );
        assert_eq!(ConcurrentCacheExpiry::expires_at(&c, &1), (false, None));
    }

    #[test]
    fn expires_at_live_entry_returns_the_stored_future_deadline() {
        let ttl = Duration::from_secs(60);
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(ttl)
            .build()
            .unwrap();
        let before = Instant::now();
        SyncConcurrentCached::cache_set(&c, 1, 100).expect("insert must succeed");
        let after = Instant::now();

        let (present, expires_at) = ConcurrentCacheExpiry::cache_expires_at(&c, &1);
        assert!(present, "a stored key must report present");
        let expires_at = expires_at.expect("a configured ttl must record a deadline");
        assert!(expires_at > Instant::now(), "a live entry expires later");
        assert!(expires_at >= before + ttl && expires_at <= after + ttl);
    }

    #[test]
    fn expires_at_never_expiring_entry_reports_present_with_no_deadline() {
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        // A zero ttl disables expiry, so the entry is stored without a deadline.
        c.unset_ttl();
        SyncConcurrentCached::cache_set(&c, 1, 100).expect("insert must succeed");
        assert_eq!(
            ConcurrentCacheExpiry::cache_expires_at(&c, &1),
            (true, None),
            "present-with-no-deadline must be distinguishable from absent by the flag"
        );
        assert_eq!(
            ConcurrentCacheExpiry::cache_expires_at(&c, &2),
            (false, None)
        );
    }

    #[test]
    fn expires_at_expired_entry_returns_a_past_deadline_and_keeps_the_entry() {
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_millis(20))
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1, 100).expect("insert must succeed");
        std::thread::sleep(std::time::Duration::from_millis(60));

        let (present, expires_at) = ConcurrentCacheExpiry::cache_expires_at(&c, &1);
        assert!(present, "an expired entry is still reported present");
        let expires_at = expires_at.expect("an expired entry still carries its deadline");
        assert!(expires_at <= Instant::now(), "the deadline is in the past");
        // Not removed by the read: a second read sees the same entry and deadline.
        assert_eq!(c.len(), 1);
        assert_eq!(
            ConcurrentCacheExpiry::cache_expires_at(&c, &1),
            (true, Some(expires_at))
        );
    }

    // The two reads must never disagree: same deadline, and the presence flag must track whether
    // the value-bearing read returned `Some`.
    #[test]
    fn expires_at_agrees_with_peek_expires_at_across_all_return_shapes() {
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_millis(20))
            .shards(4)
            .build()
            .unwrap();

        let check = |k: u32, label: &str| {
            let (value, peeked) = ConcurrentCacheExpiry::cache_peek_expires_at(&c, &k);
            let (present, deadline) = ConcurrentCacheExpiry::cache_expires_at(&c, &k);
            assert_eq!(
                present,
                value.is_some(),
                "presence flag disagrees ({label})"
            );
            assert_eq!(deadline, peeked, "deadline disagrees ({label})");
            assert_eq!(
                ConcurrentCacheExpiry::expires_at(&c, &k),
                ConcurrentCacheExpiry::cache_expires_at(&c, &k),
                "alias disagrees ({label})"
            );
            (present, deadline)
        };

        // absent
        assert_eq!(check(1, "absent"), (false, None));

        // live
        SyncConcurrentCached::cache_set(&c, 1, 100).expect("insert must succeed");
        let (present, deadline) = check(1, "live");
        assert!(present && deadline.unwrap() > Instant::now());

        // expired, not removed
        std::thread::sleep(std::time::Duration::from_millis(60));
        let (present, deadline) = check(1, "expired");
        assert!(present && deadline.unwrap() <= Instant::now());

        // never-expiring
        c.unset_ttl();
        SyncConcurrentCached::cache_set(&c, 2, 200).expect("insert must succeed");
        assert_eq!(check(2, "never-expiring"), (true, None));
    }

    #[test]
    fn expires_at_does_not_touch_hit_or_miss_counters() {
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .shards(4)
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1, 100).expect("insert must succeed");
        let metrics = c.metrics();

        let _ = ConcurrentCacheExpiry::cache_expires_at(&c, &1); // present
        let _ = ConcurrentCacheExpiry::cache_expires_at(&c, &2); // absent
        let _ = ConcurrentCacheExpiry::expires_at(&c, &1); // through the alias

        let after = c.metrics();
        assert_eq!(after.hits, metrics.hits, "the read must not count a hit");
        assert_eq!(
            after.misses, metrics.misses,
            "the read must not count a miss"
        );
        assert_eq!(
            after.evictions, metrics.evictions,
            "the read must not evict an entry"
        );
    }

    #[test]
    fn expires_at_does_not_renew_the_ttl_with_refresh_on_hit() {
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_millis(200))
            .refresh_on_hit(true)
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1, 100).expect("insert must succeed");

        let (_, first) = ConcurrentCacheExpiry::cache_expires_at(&c, &1);
        std::thread::sleep(std::time::Duration::from_millis(40));
        let (_, second) = ConcurrentCacheExpiry::cache_expires_at(&c, &1);
        assert_eq!(
            first, second,
            "the read must not renew the ttl even with refresh_on_hit enabled"
        );

        // Control: a real hit does renew, so the assertion above is not vacuous.
        assert_eq!(c.get(&1), Some(100));
        let (_, after_hit) = ConcurrentCacheExpiry::cache_expires_at(&c, &1);
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

        fn deadline<K: Hash + Eq, V>(c: &ShardedTtlCache<K, V>, k: &K) -> (bool, Option<Instant>) {
            ConcurrentCacheExpiry::cache_expires_at(c, k)
        }

        let c = ShardedTtlCache::<u32, NotClone>::builder()
            .ttl(Duration::from_secs(60))
            .shards(4)
            .build()
            .unwrap();

        // Every insert path this store exposes requires `V: Clone`, so seed the shard maps
        // directly. The read under test is what this exercises.
        let stored = Instant::now() + Duration::from_secs(60);
        c.shard_of(&1u32).lock.write().insert(
            1u32,
            TimedEntry {
                expires_at: Some(stored),
                value: NotClone(100),
            },
        );
        c.shard_of(&2u32).lock.write().insert(
            2u32,
            TimedEntry {
                expires_at: None,
                value: NotClone(200),
            },
        );

        assert_eq!(deadline(&c, &1), (true, Some(stored)), "live entry");
        assert_eq!(deadline(&c, &2), (true, None), "never-expiring entry");
        assert_eq!(deadline(&c, &3), (false, None), "absent key");
        // The alias is equally bound-free.
        assert!(ConcurrentCacheExpiry::expires_at(&c, &1).0);
        // The value was never cloned or moved out: it is still in the shard.
        assert_eq!(
            c.shard_of(&1u32).lock.read().get(&1u32).map(|e| &e.value),
            Some(&NotClone(100))
        );
    }

    // The value-free read must be reachable through a generic `T: ConcurrentCacheExpiry<K, V>`
    // bound and through the prelude, exactly like its value-bearing sibling.
    #[test]
    fn concurrent_cache_expires_at_is_reachable_through_a_generic_bound_and_the_prelude() {
        use crate::prelude::*;

        fn deadline_via_bound<T: ConcurrentCacheExpiry<u32, u32>>(
            store: &T,
            key: &u32,
        ) -> (bool, Option<crate::time::Instant>) {
            store.cache_expires_at(key)
        }

        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1, 100).expect("insert must succeed");

        let (present, expires_at) = deadline_via_bound(&c, &1);
        assert!(present);
        assert!(expires_at.is_some());
        assert_eq!(
            deadline_via_bound(&c, &2),
            (false, None),
            "absent key via the generic bound"
        );
    }

    // Multi-shard routing: the value-free read must find each key in whichever shard it landed in.
    #[test]
    fn expires_at_routes_correctly_across_many_shards() {
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(3600))
            .shards(8)
            .build()
            .unwrap();
        populate_mixed(&c);

        let distinct_shards: std::collections::HashSet<usize> = (0..24u32)
            .map(|k| c.shard_of(&k) as *const _ as usize)
            .collect();
        assert!(
            distinct_shards.len() > 1,
            "fixture must span multiple shards for this test to be meaningful"
        );

        for i in 0..8u32 {
            let (present, expires_at) = ConcurrentCacheExpiry::cache_expires_at(&c, &i);
            assert!(present, "key {i} (expired group) must report present");
            assert!(
                expires_at.unwrap() <= Instant::now(),
                "key {i} must carry a past deadline"
            );
        }
        for i in 8..16u32 {
            let (present, expires_at) = ConcurrentCacheExpiry::cache_expires_at(&c, &i);
            assert!(present, "key {i} (live group) must report present");
            assert!(
                expires_at.unwrap() > Instant::now(),
                "key {i} must carry a future deadline"
            );
        }
        for i in 16..24u32 {
            assert_eq!(
                ConcurrentCacheExpiry::cache_expires_at(&c, &i),
                (true, None),
                "key {i} (never-expiring group)"
            );
        }
        assert_eq!(
            ConcurrentCacheExpiry::cache_expires_at(&c, &999u32),
            (false, None),
            "an absent key must report absent regardless of shard count"
        );
    }

    // Physical removal must be reflected as absent, not as present-with-no-deadline.
    #[test]
    fn expires_at_reports_absent_after_removal() {
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_millis(20))
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1, 100).expect("insert must succeed");
        std::thread::sleep(std::time::Duration::from_millis(60));

        // Expired but not yet swept: still present, with a past deadline.
        let (present, expires_at) = ConcurrentCacheExpiry::cache_expires_at(&c, &1);
        assert!(present);
        assert!(expires_at.unwrap() <= Instant::now());

        assert_eq!(c.evict(), 1, "evict must remove the expired entry");
        assert_eq!(
            ConcurrentCacheExpiry::cache_expires_at(&c, &1),
            (false, None),
            "a physically removed entry must be reported absent"
        );

        SyncConcurrentCached::cache_set(&c, 2, 200).expect("insert must succeed");
        assert_eq!(c.remove(&2), Some(200));
        assert_eq!(
            ConcurrentCacheExpiry::cache_expires_at(&c, &2),
            (false, None)
        );
    }
}
