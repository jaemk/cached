use std::hash::Hash;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use crate::{CacheMetrics, CachedIter, ConcurrentCacheBase, ConcurrentCachePeek, ConcurrentCached};
#[cfg(feature = "async_core")]
use crate::{ConcurrentCachePeekAsync, ConcurrentCachedAsync};
#[cfg(feature = "async_core")]
use core::future::Future;

use super::{
    CachePadded, DefaultShardHasher, Shard, ShardHasher, checked_per_shard_cap_from_total,
    checked_shard_count, default_shard_count_for_capacity, per_shard_cap_from_total, shard_index,
};
use crate::stores::{BuildError, LruCache};

type OnEvict<K, V> = Arc<dyn Fn(&K, &V) + Send + Sync>;

#[allow(clippy::type_complexity)]
struct LruInner<K, V, H> {
    shards: Box<[CachePadded<Shard<LruCache<K, V>>>]>,
    shard_mask: usize,
    hasher: H,
    on_evict: Option<OnEvict<K, V>>,
    /// Total logical capacity (sum of per-shard caps). Stored as `AtomicUsize` so
    /// [`set_max_size`](ShardedLruCache::set_max_size) can update it from `&self`.
    total_capacity: AtomicUsize,
}

/// A fully-concurrent, partitioned, LRU-bounded in-memory cache.
///
/// Wraps an `Arc` — `clone()` is an Arc-share (shared state), not a deep copy.
/// Use [`deep_clone`](ShardedLruCache::deep_clone) to get an independent copy.
///
/// The shard-selection hasher `H` defaults to [`DefaultShardHasher`] (ahash-backed when the
/// `ahash` feature is enabled, otherwise `std::collections::hash_map::RandomState`), so
/// `ShardedLruCache<K, V>` names the common case. To use a custom [`ShardHasher`], call
/// [`ShardedLruCache::builder()`] and then [`hasher`](ShardedLruCacheBuilder::hasher), which
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
///
/// **Note**: LRU promotion requires mutable access to the per-shard store, so
/// `cache_get` acquires a **write** lock (unlike `ShardedUnboundCache` which only needs a read lock).
/// Under many concurrent readers this can be a bottleneck; consider `ShardedUnboundCache` if you do
/// not need capacity bounding. This write-lock-on-read behavior is a known limitation of the
/// strict-LRU sharded stores. A future read-optimized variant that relaxes strict recency ordering
/// will ship as a separate store type; the existing stores will not change semantics.
///
/// **Note**: `K` must implement `Clone` (needed for LRU key tracking). `ShardedUnboundCache<K, V>`
/// requires only `K: Hash + Eq`. `V` must also implement `Clone`, because reads return owned
/// values cloned from under the shard lock.
///
/// **Note**: Setting an `on_evict` callback requires the callback itself to be `'static` because
/// the cache stores it behind an `Arc<dyn Fn(&K, &V) + Send + Sync>`. This does not add `'static`
/// bounds to `K` or `V`.
pub struct ShardedLruCache<K, V, H = DefaultShardHasher> {
    inner: Arc<LruInner<K, V, H>>,
}

impl<K, V, H> Clone for ShardedLruCache<K, V, H> {
    /// Arc-share clone — both handles point to the same underlying cache.
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<K, V, H> std::fmt::Debug for ShardedLruCache<K, V, H> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShardedLruCache")
            .field("shards", &self.inner.shards.len())
            .field(
                "capacity",
                &self.inner.total_capacity.load(Ordering::Relaxed),
            )
            .finish_non_exhaustive()
    }
}

impl<K, V> ShardedLruCache<K, V, DefaultShardHasher>
where
    K: Hash + Eq + Clone,
{
    /// Construct a ready-to-use [`ShardedLruCache`] holding up to roughly `max_size`
    /// entries total, with the [`DefaultShardHasher`] and a default shard count.
    ///
    /// Note that the effective total capacity can still exceed `max_size` for small values
    /// because each shard reserves a minimum capacity (see
    /// [`max_size`](ShardedLruCacheBuilder::max_size)). The default shard count is now scaled
    /// down for small `max_size` (roughly `max_size / 16` shards, capped by the CPU-derived
    /// default), so the overshoot is modest -- `max_size = 100` yields 8 shards x 16 = 128
    /// effective capacity rather than the full default shard count times 16. For a custom
    /// hasher, shard count, per-shard cap, or `on_evict`, use [`builder`](Self::builder).
    ///
    /// # Panics
    ///
    /// Panics if `max_size` is `0`, or if the effective sharded capacity overflows
    /// `usize` / a per-shard allocation fails. Use [`builder`](Self::builder) with
    /// [`build`](ShardedLruCacheBuilder::build) to handle those cases without panicking.
    #[must_use]
    pub fn new(max_size: usize) -> ShardedLruCache<K, V> {
        Self::builder()
            .max_size(max_size)
            .build()
            .expect("ShardedLruCache::new requires a non-zero max_size with a valid allocation")
    }

    /// Return a builder for constructing a [`ShardedLruCache`].
    ///
    /// The builder starts with the [`DefaultShardHasher`]. To use a custom hasher, call
    /// [`hasher`](ShardedLruCacheBuilder::hasher) on the returned builder; it switches the
    /// builder's hasher type and `build` then yields a `ShardedLruCache<K, V, H>` over that
    /// hasher. `new` and `builder` exist only on the default-hasher instantiation
    /// `ShardedLruCache<K, V, DefaultShardHasher>`, so a custom hasher is always introduced
    /// via `hasher`, never a `ShardedLruCache::<_, _, H>` turbofish.
    #[must_use]
    pub fn builder() -> ShardedLruCacheBuilder<K, V, DefaultShardHasher> {
        ShardedLruCacheBuilder::default()
    }
}

impl<K, V, H> ShardedLruCache<K, V, H>
where
    K: Hash + Eq + Clone,
    H: ShardHasher<K>,
{
    #[inline]
    fn shard_of(&self, k: &K) -> &CachePadded<Shard<LruCache<K, V>>> {
        let h = self.inner.hasher.shard_hash(k);
        &self.inner.shards[shard_index(h, self.inner.shard_mask)]
    }
}

impl<K: Clone + Hash + Eq, V: Clone, H: ShardHasher<K>> ShardedLruCache<K, V, H> {
    /// Return an independent deep copy of this cache — entries and metrics are
    /// duplicated, not shared. In most cases [`Clone::clone`] (Arc-share) is
    /// what you want.
    #[must_use]
    pub fn deep_clone(&self) -> Self {
        let n = self.inner.shards.len();
        let shards = (0..n)
            .map(|i| {
                let guard = self.inner.shards[i].lock.read();
                let store_copy = guard.clone();
                let hits = self.inner.shards[i].hits.load(Ordering::Relaxed);
                let misses = self.inner.shards[i].misses.load(Ordering::Relaxed);
                drop(guard);
                let shard = Shard {
                    lock: parking_lot::RwLock::new(store_copy),
                    hits: AtomicU64::new(hits),
                    misses: AtomicU64::new(misses),
                    evictions: AtomicU64::new(0),
                };
                CachePadded(shard)
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            inner: Arc::new(LruInner {
                shards,
                shard_mask: self.inner.shard_mask,
                hasher: self.inner.hasher.clone(),
                on_evict: self.inner.on_evict.clone(),
                total_capacity: AtomicUsize::new(self.inner.total_capacity.load(Ordering::Relaxed)),
            }),
        }
    }
}

impl<K, V, H: ShardHasher<K>> ShardedLruCache<K, V, H>
where
    K: Hash + Eq + Clone,
    V: Clone,
{
    /// Retrieve a cached value, returning `None` on a miss.
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

    /// Return true if a live value is stored for `k`. Peek-based: no recency update, no hit/miss metrics.
    #[must_use]
    pub fn contains(&self, k: &K) -> bool {
        ConcurrentCached::cache_contains(self, k).unwrap()
    }

    /// Return a clone of the value stored for `k` without observable side effects:
    /// no LRU recency update, no hit/miss metrics. The single-owner counterpart is
    /// [`CachedPeek::cache_peek`](crate::CachedPeek::cache_peek); the sharded stores
    /// return a clone rather than a reference because the value lives behind a
    /// per-shard lock.
    #[must_use]
    pub fn peek(&self, k: &K) -> Option<V> {
        use crate::CachedPeek;
        self.shard_of(k).lock.read().cache_peek(k).cloned()
    }
}

impl<K, V, H: ShardHasher<K>> ShardedLruCache<K, V, H>
where
    K: Hash + Eq + Clone,
{
    /// Return aggregate metrics across all shards.
    ///
    /// `evictions` counts both LRU capacity evictions (tracked per-shard) and
    /// explicit removes via [`ConcurrentCached::cache_remove`].
    /// `capacity` reflects the effective total capacity — may exceed the requested
    /// `size` when the 16-per-shard minimum floor is applied; see [`capacity`](Self::capacity).
    ///
    /// Approximate under concurrent mutation: no global lock is held across shards; each shard is
    /// locked and read one at a time.
    #[must_use]
    pub fn metrics(&self) -> CacheMetrics {
        let mut hits = 0u64;
        let mut misses = 0u64;
        let mut evictions = 0u64;
        let mut size = 0usize;
        for shard in self.inner.shards.iter() {
            hits += shard.hits.load(Ordering::Relaxed);
            misses += shard.misses.load(Ordering::Relaxed);
            let guard = shard.lock.read();
            if let Some(e) = guard.cache_evictions() {
                evictions += e;
            }
            size += guard.cache_size();
        }

        CacheMetrics {
            hits: Some(hits),
            misses: Some(misses),
            evictions: Some(evictions),
            entry_count: Some(size),
            // Acquire, like `capacity()`: a caller that just resized on this thread sees
            // the new total here too, not a stale value alongside a fresh `capacity()`.
            capacity: Some(self.inner.total_capacity.load(Ordering::Acquire)),
        }
    }

    /// Number of shards.
    #[must_use]
    pub fn shards(&self) -> usize {
        self.inner.shards.len()
    }

    /// Per-shard live entry counts — useful for diagnosing key distribution skew.
    #[must_use]
    pub fn shard_sizes(&self) -> Vec<usize> {
        self.inner
            .shards
            .iter()
            .map(|s| s.lock.read().cache_size())
            .collect()
    }

    /// Total number of live entries across all shards.
    ///
    /// Approximate under concurrent mutation: no global lock is held across shards; each shard is
    /// locked and read one at a time.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner
            .shards
            .iter()
            .map(|s| s.lock.read().cache_size())
            .sum()
    }

    /// `true` if no entries are present.
    ///
    /// Approximate under concurrent mutation: no global lock is held across shards; each shard is
    /// locked and read one at a time.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner
            .shards
            .iter()
            .all(|s| s.lock.read().cache_size() == 0)
    }

    /// Remove all entries from every shard. Does **not** fire `on_evict`.
    /// Use [`cache_clear_with_on_evict`](Self::cache_clear_with_on_evict) to opt into callback firing.
    pub fn clear(&self) {
        for shard in self.inner.shards.iter() {
            shard.lock.write().cache_clear();
        }
    }

    /// Remove all entries from every shard, firing `on_evict` for each removed entry when a
    /// callback is configured.
    ///
    /// Unlike [`clear`](Self::clear), every removed entry is counted as an eviction
    /// (`metrics().evictions`) whether or not an `on_evict` callback is configured; the callback
    /// fires only when one is set.
    pub fn cache_clear_with_on_evict(&self) {
        for shard in self.inner.shards.iter() {
            let removed: Vec<(K, V)> = {
                let mut guard = shard.lock.write();
                // `drain_all` walks each shard's LRU chain once taking owned pairs in
                // MRU -> LRU order -- the same order the old "clone every key, then
                // `pop_raw` each one" drain fired in, but with zero key clones and zero
                // re-hashing.
                let removed = guard.drain_all();
                if !removed.is_empty() {
                    guard
                        .evictions
                        .fetch_add(removed.len() as u64, Ordering::Relaxed);
                }
                removed
            };
            if let Some(on_evict) = &self.inner.on_evict {
                for (k, v) in &removed {
                    on_evict(k, v);
                }
            }
        }
    }

    /// Remove every entry for which `keep` returns `false`, firing `on_evict` (if configured) and
    /// incrementing `evictions` once per removed entry — matching
    /// [`LruCache::retain`](crate::LruCache::retain) semantics. The LRU recency order of the
    /// surviving entries in each shard is unchanged.
    ///
    /// Returns the total number of entries removed across all shards for this call. Not
    /// `#[must_use]`: discarding the count is a legitimate and common use.
    ///
    /// Shards are processed one at a time under their own write lock, so this is **not atomic**
    /// across shards: a concurrent reader may observe some shards already filtered and others not
    /// yet touched. `keep` runs while the affected shard's write lock is held — do not call
    /// methods on this same cache from inside `keep`, as re-entering the locked shard can
    /// deadlock. `on_evict` fires after the shard's write lock has been released, once per removed
    /// entry, in shard order (and in each shard's iteration order for that shard's removals).
    /// Because callbacks run between shard sweeps, an `on_evict` that inserts into a shard this
    /// call has not yet visited will have that entry filtered by the same in-flight `retain`.
    pub fn retain<F: FnMut(&K, &V) -> bool>(&self, mut keep: F) -> usize {
        let mut total_removed = 0usize;
        for shard in self.inner.shards.iter() {
            let removed: Vec<(K, V)> = {
                let mut guard = shard.lock.write();
                let doomed: Vec<K> = guard
                    .iter()
                    .filter_map(|(k, v)| if keep(k, v) { None } else { Some(k.clone()) })
                    .collect();
                let mut removed = Vec::with_capacity(doomed.len());
                for k in doomed {
                    if let Some(pair) = guard.pop_raw(&k) {
                        removed.push(pair);
                    }
                }
                if !removed.is_empty() {
                    guard
                        .evictions
                        .fetch_add(removed.len() as u64, Ordering::Relaxed);
                }
                removed
            };
            total_removed += removed.len();
            if let Some(on_evict) = &self.inner.on_evict {
                for (k, v) in &removed {
                    on_evict(k, v);
                }
            }
        }
        total_removed
    }

    /// Effective total capacity across all shards.
    ///
    /// When constructed with [`max_size`](ShardedLruCacheBuilder::max_size), this may
    /// be larger than the requested size because per-shard capacity is rounded
    /// up with ceiling division.
    #[doc(alias = "max_size")]
    #[must_use]
    pub fn capacity(&self) -> usize {
        // Acquire pairs with the Release swap in `set_max_size`: observing a new
        // total implies every shard has already adopted its new per-shard cap.
        self.inner.total_capacity.load(Ordering::Acquire)
    }

    /// Resize the cache to hold up to `max_size` entries in total, returning
    /// the previous total capacity as `Some(prev)`. The return is always `Some`;
    /// the `Option` wrapper mirrors the single-owner
    /// [`LruCache::set_max_size`](crate::LruCache::set_max_size) signature.
    ///
    /// Takes `&self`: shards use interior mutability (per-shard write locks), so
    /// the method is callable through `Arc` or any shared reference — no external
    /// lock is needed, unlike the `&mut self` single-owner counterpart.
    ///
    /// The new per-shard capacity is recomputed using the same policy the builder
    /// uses for [`max_size`](ShardedLruCacheBuilder::max_size): ceiling division
    /// across shards with a minimum of 16 entries per shard when `shards > 1`.
    /// After resizing, any configuration previously set via
    /// [`per_shard_max_size`](ShardedLruCacheBuilder::per_shard_max_size) is replaced
    /// by the total-based policy.
    ///
    /// On shrink, excess LRU entries are evicted per shard: `on_evict` fires for
    /// each evicted entry and the eviction counter is incremented accordingly.
    /// On grow, no pre-allocation occurs; the shards grow on demand.
    ///
    /// The resize is **not atomic** across shards: shards are locked one at a time
    /// (write lock), so concurrent readers may briefly observe mixed capacities
    /// across shards while the resize is in progress. The new total reported by
    /// [`capacity`](Self::capacity) is published only after every shard has adopted
    /// its new per-shard cap.
    ///
    /// The same applies to **concurrent callers** of `set_max_size`: two overlapping
    /// resizes interleave their per-shard writes, so individual shards can end up
    /// with a mix of the two targets while `capacity()` reports whichever total was
    /// published last. No entries are lost and there is no data race, but the
    /// resulting bound is a blend of the two requests. Serialize resizes externally
    /// (or re-issue the desired resize) if a single consistent target matters.
    ///
    /// When the 16-per-shard minimum floor applies (small `max_size` with multiple
    /// shards), `capacity()` after the call reflects the clamped total, which may
    /// exceed the requested `max_size` (e.g. `set_max_size(4)` on a 16-shard cache
    /// yields `capacity() == 256`).
    ///
    /// # Panics
    ///
    /// Panics if `max_size` is 0. Also panics if `max_size` is close enough to `usize::MAX`
    /// that dividing it across the shard count and multiplying back overflows `usize` (only
    /// reachable on a multi-shard cache); see
    /// [`SetMaxSizeError::CapacityOverflow`](crate::SetMaxSizeError::CapacityOverflow). Use
    /// [`try_set_max_size`](ShardedLruCache::try_set_max_size) to avoid either panic.
    ///
    /// # See also
    ///
    /// [`ShardedLruTtlCache::set_max_size`](crate::ShardedLruTtlCache::set_max_size) and
    /// [`ShardedExpiringLruCache::set_max_size`](crate::ShardedExpiringLruCache::set_max_size)
    /// are the parallel methods on the other sharded LRU-bounded stores.
    pub fn set_max_size(&self, max_size: usize) -> Option<usize> {
        assert!(max_size > 0, "max_size must be greater than zero");
        let n_shards = self.inner.shards.len();
        let (per_shard_cap, total_cap) = per_shard_cap_from_total(max_size, n_shards);
        for shard in self.inner.shards.iter() {
            shard.lock.write().set_max_size(per_shard_cap);
        }
        // Publish the new total only after every shard has adopted its new cap;
        // Release pairs with the Acquire load in `capacity()`.
        let prev = self.inner.total_capacity.swap(total_cap, Ordering::Release);
        Some(prev)
    }

    /// Fallible counterpart of [`set_max_size`](ShardedLruCache::set_max_size): validates
    /// that `max_size` is non-zero and then delegates to `set_max_size`.
    /// Returns the previous total capacity wrapped in `Some` on success.
    ///
    /// # Errors
    ///
    /// Returns [`SetMaxSizeError::ZeroMaxSize`](crate::SetMaxSizeError) if `max_size` is 0.
    /// Returns [`SetMaxSizeError::CapacityOverflow`](crate::SetMaxSizeError) if `max_size` is
    /// close enough to `usize::MAX` that dividing it across the shard count and multiplying
    /// back overflows `usize`.
    pub fn try_set_max_size(
        &self,
        max_size: usize,
    ) -> Result<Option<usize>, crate::SetMaxSizeError> {
        if max_size == 0 {
            return Err(crate::SetMaxSizeError::ZeroMaxSize);
        }
        checked_per_shard_cap_from_total(max_size, self.inner.shards.len())?;
        Ok(self.set_max_size(max_size))
    }
}

use crate::Cached;

impl<K, V, H> ConcurrentCacheBase for ShardedLruCache<K, V, H>
where
    K: Hash + Eq + Clone,
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

    fn cache_capacity(&self) -> Option<usize> {
        // Acquire: see `capacity()`.
        Some(self.inner.total_capacity.load(Ordering::Acquire))
    }

    fn cache_evictions(&self) -> Option<u64> {
        let mut evictions = 0u64;
        for shard in self.inner.shards.iter() {
            let guard = shard.lock.read();
            if let Some(e) = guard.cache_evictions() {
                evictions += e;
            }
        }
        Some(evictions)
    }
}

impl<K, V, H> ConcurrentCached<K, V> for ShardedLruCache<K, V, H>
where
    K: Hash + Eq + Clone,
    V: Clone,
    H: ShardHasher<K>,
{
    fn cache_get(&self, k: &K) -> Result<Option<V>, Self::Error> {
        let shard = self.shard_of(k);
        let mut guard = shard.lock.write();
        let value = guard.cache_get(k).cloned();
        // Release the shard lock before touching the counters: the atomics are
        // shard-local but there is no reason to hold the write lock across them
        // (the `drop(guard)`-first pattern used by the other sharded stores).
        drop(guard);
        match value {
            Some(v) => {
                shard.hits.fetch_add(1, Ordering::Relaxed);
                Ok(Some(v))
            }
            None => {
                shard.misses.fetch_add(1, Ordering::Relaxed);
                Ok(None)
            }
        }
    }

    fn cache_set(&self, k: K, v: V) -> Result<Option<V>, Self::Error> {
        let shard = self.shard_of(&k);
        Ok(shard.lock.write().cache_set(k, v))
    }

    fn cache_remove(&self, k: &K) -> Result<Option<V>, Self::Error> {
        ConcurrentCached::cache_remove_entry(self, k).map(|r| r.map(|(_, v)| v))
    }

    fn cache_remove_entry(&self, k: &K) -> Result<Option<(K, V)>, Self::Error> {
        let shard = self.shard_of(k);
        let removed = {
            let mut guard = shard.lock.write();
            let removed = guard.pop_raw(k);
            if removed.is_some() {
                guard.evictions.fetch_add(1, Ordering::Relaxed);
            }
            removed
        };
        if let Some((ref key, ref value)) = removed
            && let Some(on_evict) = &self.inner.on_evict
        {
            on_evict(key, value);
        }
        Ok(removed)
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
            // Zero the per-shard inner store's metrics, including its eviction counter.
            shard.lock.write().cache_reset_metrics();
        }
        Ok(())
    }

    /// Efficient peek-based contains: acquires a read lock, does not clone the value,
    /// does not update LRU recency, and does not record hit/miss metrics.
    fn cache_contains(&self, k: &K) -> Result<bool, Self::Error> {
        use crate::CachedPeek;
        let shard = self.shard_of(k);
        Ok(shard.lock.read().cache_peek(k).is_some())
    }
}

impl<K, V, H> ConcurrentCachePeek<K, V> for ShardedLruCache<K, V, H>
where
    K: Hash + Eq + Clone,
    V: Clone,
    H: ShardHasher<K>,
{
    fn cache_peek(&self, k: &K) -> Result<Option<V>, Self::Error> {
        Ok(self.peek(k))
    }
}

#[cfg(feature = "async_core")]
#[cfg_attr(docsrs, doc(cfg(feature = "async_core")))]
impl<K, V, H> ConcurrentCachePeekAsync<K, V> for ShardedLruCache<K, V, H>
where
    K: Hash + Eq + Clone + Send + Sync,
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
impl<K, V, H> ConcurrentCachedAsync<K, V> for ShardedLruCache<K, V, H>
where
    K: Hash + Eq + Clone + Send + Sync,
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

    /// Efficient peek-based contains: does not clone the value, does not update LRU
    /// recency, and does not record hit/miss metrics.
    fn async_cache_contains(&self, k: &K) -> impl Future<Output = Result<bool, Self::Error>> + Send
    where
        Self: Sized + Sync,
        K: Sync,
    {
        let result = ConcurrentCached::cache_contains(self, k);
        async move { result }
    }
}

/// Builder for [`ShardedLruCache`].
pub struct ShardedLruCacheBuilder<K, V, H = DefaultShardHasher> {
    shards: Option<usize>,
    max_size: Option<usize>,
    per_shard_max_size: Option<usize>,
    hasher: Option<H>,
    on_evict: Option<OnEvict<K, V>>,
    _k: std::marker::PhantomData<K>,
    _v: std::marker::PhantomData<V>,
}

impl<K, V> Default for ShardedLruCacheBuilder<K, V, DefaultShardHasher> {
    fn default() -> Self {
        Self {
            shards: None,
            max_size: None,
            per_shard_max_size: None,
            hasher: Some(DefaultShardHasher::default()),
            on_evict: None,
            _k: std::marker::PhantomData,
            _v: std::marker::PhantomData,
        }
    }
}

impl<K, V> ShardedLruCacheBuilder<K, V> {
    /// Create a builder with default settings. Equivalent to [`ShardedLruCache::builder`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl<K, V, H> ShardedLruCacheBuilder<K, V, H> {
    /// Set the requested total capacity (divided across shards via `div_ceil`).
    ///
    /// Eviction is enforced independently per shard. Each shard gets
    /// `ceil(size / shards)` entries, with a minimum of 16 per shard when
    /// `shards > 1` to avoid capacity fragmentation/eviction flakes.
    ///
    /// # Minimum capacity
    ///
    /// Because each shard reserves a minimum of **16** entries when `shards > 1`, the effective
    /// total capacity is at least `shards * 16` and may **exceed** the requested `max_size` for
    /// small values. On the default shard-count path the shard count itself is scaled down for
    /// small `max_size` (roughly `max_size / 16`, capped by the CPU-derived default), so the
    /// overshoot stays modest: `max_size = 100` builds 8 shards x 16 = 128 effective capacity.
    /// An explicit [`shards`](Self::shards) count opts out of that scaling, so a large explicit
    /// count with a small `max_size` still overshoots by `shards * 16` (e.g. `max_size = 10`
    /// with an explicit 8 shards yields 128).
    /// [`metrics()`](ShardedLruCache::metrics)'s `capacity` and `entry_count` reflect the
    /// actual (possibly larger) amount. Use [`per_shard_max_size`](Self::per_shard_max_size) or
    /// `shards = 1` if you need a strict small cap.
    ///
    /// Use [`per_shard_max_size`](Self::per_shard_max_size) for an exact per-shard cap.
    /// Mutually exclusive with [`per_shard_max_size`](Self::per_shard_max_size).
    #[doc(alias = "size")]
    #[doc(alias = "capacity")]
    #[must_use]
    pub fn max_size(mut self, max_size: usize) -> Self {
        self.max_size = Some(max_size);
        self
    }

    /// Set per-shard capacity directly. Advanced — bypasses the automatic
    /// division. Mutually exclusive with [`max_size`](Self::max_size).
    #[must_use]
    pub fn per_shard_max_size(mut self, per_shard_max_size: usize) -> Self {
        self.per_shard_max_size = Some(per_shard_max_size);
        self
    }

    /// Set the number of shards (rounded up to the next power of two).
    #[must_use]
    pub fn shards(mut self, shards: usize) -> Self {
        self.shards = Some(shards);
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
    pub fn hasher<H2: ShardHasher<K>>(self, hasher: H2) -> ShardedLruCacheBuilder<K, V, H2> {
        ShardedLruCacheBuilder {
            shards: self.shards,
            max_size: self.max_size,
            per_shard_max_size: self.per_shard_max_size,
            hasher: Some(hasher),
            on_evict: self.on_evict,
            _k: std::marker::PhantomData,
            _v: std::marker::PhantomData,
        }
    }

    /// Set a callback invoked when an entry is evicted. Fires in four situations:
    /// on LRU capacity pressure; on explicit
    /// [`cache_remove`](ConcurrentCached::cache_remove); on
    /// [`cache_remove_entry`](ConcurrentCached::cache_remove_entry); and for every
    /// entry removed by
    /// [`cache_clear_with_on_evict`](ShardedLruCache::cache_clear_with_on_evict).
    /// Does **not** fire on [`clear`](ShardedLruCache::clear).
    ///
    /// Capacity-eviction callbacks run while the affected shard's write lock is held. Do not call
    /// methods on the same sharded cache from the callback; doing so can deadlock if the callback
    /// re-enters the locked shard.
    ///
    /// The closure must be `'static` (its captures cannot borrow from the local stack), but `K`
    /// and `V` themselves are not required to be `'static`.
    #[must_use]
    pub fn on_evict(mut self, on_evict: impl Fn(&K, &V) + Send + Sync + 'static) -> Self {
        self.on_evict = Some(Arc::new(on_evict));
        self
    }

    fn resolve_per_shard_cap(&self, n_shards: usize) -> Result<usize, BuildError> {
        match (self.max_size, self.per_shard_max_size) {
            (Some(_), Some(_)) => Err(BuildError::InvalidValue {
                field: "max_size / per_shard_max_size",
                reason: "`max_size` and `per_shard_max_size` are mutually exclusive",
            }),
            (None, None) => Err(BuildError::MissingRequired("max_size")),
            (Some(total), None) => {
                if total == 0 {
                    return Err(BuildError::InvalidValue {
                        field: "max_size",
                        reason: "must be greater than zero",
                    });
                }
                let mut cap = total.div_ceil(n_shards);
                if n_shards > 1 {
                    // Enforce a minimum capacity of 16 per shard to avoid capacity fragmentation/eviction flakes
                    cap = std::cmp::max(cap, 16);
                }
                Ok(cap)
            }
            (None, Some(per)) => {
                if per == 0 {
                    return Err(BuildError::InvalidValue {
                        field: "per_shard_max_size",
                        reason: "must be greater than zero",
                    });
                }
                Ok(per)
            }
        }
    }

    fn total_capacity(&self, n_shards: usize, per_shard_cap: usize) -> Result<usize, BuildError> {
        // Name the attribute the user actually set so the diagnostic points at the
        // right knob (`per_shard_max_size` multiplies by shard count; `max_size` does not).
        let field = if self.per_shard_max_size.is_some() {
            "per_shard_max_size"
        } else {
            "max_size"
        };
        n_shards
            .checked_mul(per_shard_cap)
            .ok_or(BuildError::InvalidValue {
                field,
                reason: "effective sharded capacity overflows usize",
            })
    }

    /// Resolve the shard count for this build.
    ///
    /// An explicit `.shards(n)` is authoritative: it goes through
    /// [`checked_shard_count`], which rounds up to a power of two, rejects `Some(0)`, and
    /// guards against the rounding overflowing `usize`.
    ///
    /// On the default path (no explicit shard count) the count is derived from capacity via
    /// [`default_shard_count_for_capacity`]: a configured total `max_size` scales the default
    /// shard count down toward `max_size / 16` so a small cache does not preallocate an
    /// oversized shard array. The `per_shard_max_size` path has no total to scale against, so
    /// `self.max_size` is `None` there and the plain `default_shard_count()` is kept.
    fn resolve_shard_count(&self) -> Result<usize, BuildError> {
        match self.shards {
            Some(_) => checked_shard_count(self.shards),
            None => Ok(default_shard_count_for_capacity(self.max_size)),
        }
    }

    /// Build the cache, returning an error if required fields are missing or invalid.
    ///
    /// Use [`ShardedLruCache::builder()`] to obtain
    /// a builder, set at least [`max_size`](Self::max_size) or
    /// [`per_shard_max_size`](Self::per_shard_max_size), then call `.build()`.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError`] if `max_size` (or `per_shard_max_size`) was not set, is `0`,
    /// or if both `max_size` and `per_shard_max_size` are set simultaneously, or if the
    /// effective sharded capacity overflows `usize`.
    #[must_use = "the Result from build() must be used"]
    pub fn build(self) -> Result<ShardedLruCache<K, V, H>, BuildError>
    where
        K: Hash + Eq + Clone,
        H: ShardHasher<K>,
    {
        let n = self.resolve_shard_count()?;
        let mask = n - 1;
        let per_shard_cap = self.resolve_per_shard_cap(n)?;
        let total_cap = self.total_capacity(n, per_shard_cap)?;
        let on_evict = self.on_evict.clone();
        let shards = (0..n)
            .map(|_| {
                let mut lru = LruCache::builder().max_size(per_shard_cap).build()?;
                lru.on_evict = on_evict.clone();
                lru.disable_hit_miss_tracking();
                Ok(CachePadded(Shard::new(lru)))
            })
            .collect::<Result<Vec<_>, BuildError>>()?
            .into_boxed_slice();
        Ok(ShardedLruCache {
            inner: Arc::new(LruInner {
                shards,
                shard_mask: mask,
                hasher: self
                    .hasher
                    .expect("hasher is always initialized via Default or .hasher()"),
                on_evict: self.on_evict,
                total_capacity: AtomicUsize::new(total_cap),
            }),
        })
    }

    /// Build the new cache and copy every entry from `existing` into it,
    /// preserving per-shard LRU ordering (least-recently-used entries inserted
    /// first so that most-recently-used entries end up at the head of each
    /// shard). After resharding, global recency rank across all shards is not
    /// guaranteed to be preserved.
    ///
    /// Acquires each shard's read lock on `existing` one at a time — `existing`
    /// keeps serving concurrent ops throughout. Entries that cannot fit in the
    /// new per-shard capacity are evicted (LRU-first), firing `on_evict` on the
    /// NEW cache's callback if set.
    ///
    /// **Note**: `on_evict` callbacks on `existing` do not fire — entries are read
    /// (not removed) from the source cache.
    ///
    /// # Errors
    ///
    /// Returns [`Err(BuildError)`](crate::stores::BuildError) if the builder
    /// configuration is invalid (the same conditions as [`build`](Self::build)).
    #[must_use = "the Result from copy_from() must be used"]
    pub fn copy_from<H2: ShardHasher<K>>(
        self,
        existing: &ShardedLruCache<K, V, H2>,
    ) -> Result<ShardedLruCache<K, V, H>, BuildError>
    where
        K: Clone + Hash + Eq,
        V: Clone,
        H: ShardHasher<K>,
    {
        let new_cache = self.build()?;
        for shard in existing.inner.shards.iter() {
            // iter_order returns MRU-first; insert in reverse (LRU-first)
            // so that the MRU entries are pushed in last and land at the head.
            let entries: Vec<(K, V)> = {
                let guard = shard.lock.read();
                guard.iter_order_raw()
            };
            for (k, v) in entries.into_iter().rev() {
                let _ = ConcurrentCached::cache_set(&new_cache, k, v);
            }
        }
        Ok(new_cache)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ConcurrentCached;
    use crate::ConcurrentCached as SyncConcurrentCached;

    #[test]
    fn new_returns_ready_cache_respecting_max_size() {
        // shards(1) gives an exact cap so the eviction bound is deterministic.
        let c = ShardedLruCache::<u32, u32>::builder()
            .shards(1)
            .max_size(2)
            .build()
            .unwrap();
        assert_eq!(SyncConcurrentCached::cache_set(&c, 1, 10).unwrap(), None);
        assert_eq!(SyncConcurrentCached::cache_get(&c, &1).unwrap(), Some(10));
        SyncConcurrentCached::cache_set(&c, 2, 20).unwrap();
        SyncConcurrentCached::cache_set(&c, 3, 30).unwrap(); // evicts LRU (1)
        assert_eq!(c.len(), 2);
        assert_eq!(SyncConcurrentCached::cache_get(&c, &1).unwrap(), None);

        // The inherent `new` constructor returns a ready cache too.
        let c2 = ShardedLruCache::<u32, u32>::new(64);
        assert_eq!(SyncConcurrentCached::cache_set(&c2, 1, 100).unwrap(), None);
        assert_eq!(SyncConcurrentCached::cache_get(&c2, &1).unwrap(), Some(100));

        // `new(N)` must forward N to the builder — capacity must equal the builder path.
        assert_eq!(
            ShardedLruCache::<u32, u32>::new(1024).capacity(),
            ShardedLruCache::<u32, u32>::builder()
                .max_size(1024)
                .build()
                .unwrap()
                .capacity()
        );
    }

    #[test]
    fn default_shard_count_scales_with_max_size() {
        use crate::stores::sharded::{default_shard_count, default_shard_count_for_capacity};
        // On the default path (no explicit .shards), a total max_size caps the shard count.
        let c = ShardedLruCache::<u32, u32>::builder()
            .max_size(100)
            .build()
            .unwrap();
        let expected = default_shard_count_for_capacity(Some(100));
        assert_eq!(c.shards(), expected);
        // 100/16 == 6, next_power_of_two(6) == 8, clamped into [1, default_shard_count()].
        // default_shard_count()'s documented minimum is 8, so this resolves to 8 on any host.
        assert_eq!(expected, 8usize.clamp(1, default_shard_count()));
        // capacity() reflects the capped count times the 16-per-shard floor.
        assert_eq!(c.capacity(), c.shards() * 16);
        // `new(max_size)` goes through the same default path.
        assert_eq!(ShardedLruCache::<u32, u32>::new(100).shards(), expected);
    }

    #[test]
    fn explicit_shards_override_capacity_default() {
        // An explicit .shards(n) is authoritative regardless of max_size: rounded up to a power
        // of two, never scaled down by the capacity helper.
        let c = ShardedLruCache::<u32, u32>::builder()
            .shards(64)
            .max_size(100)
            .build()
            .unwrap();
        assert_eq!(c.shards(), 64);
        // A tiny max_size with a large explicit shard count still overshoots by shards * 16.
        assert_eq!(c.capacity(), 64 * 16);
    }

    #[test]
    fn per_shard_max_size_keeps_plain_default_shard_count() {
        use crate::stores::sharded::default_shard_count;
        // The per_shard_max_size path has no total to scale against, so it keeps the plain
        // default_shard_count().
        let c = ShardedLruCache::<u32, u32>::builder()
            .per_shard_max_size(4)
            .build()
            .unwrap();
        assert_eq!(c.shards(), default_shard_count());
    }

    #[test]
    fn spec_0037_formula_holds_end_to_end() {
        // Hand-computed expected shard counts for several concrete max_size inputs -- NOT a
        // re-derivation of the production formula `(max_size / 16).next_power_of_two().clamp(1,
        // default_shard_count())`. Re-typing that same expression here (as an earlier version of
        // this test did) is byte-for-byte identical to the production code, so a shared
        // arithmetic bug would sail through both sides unnoticed; hardcoded expectations don't
        // have that blind spot.
        //
        // Every expected value below is <= 8, which is safe to hardcode regardless of the host's
        // `default_shard_count()` (documented minimum is 8, see `default_shard_count()`): the
        // upper clamp bound can only ever raise these results, never lower them, so it never
        // enters into the numbers below. The upper-clamp behavior itself is covered separately by
        // `default_shard_count_clamps_at_upper_bound_end_to_end`.
        let cases: [(usize, usize); 9] = [
            (1, 1),   // 1 / 16 == 0, next_power_of_two(0) == 1
            (15, 1),  // 15 / 16 == 0 (truncating division)
            (16, 1),  // 16 / 16 == 1, next_power_of_two(1) == 1
            (17, 1),  // 17 / 16 == 1 (truncating division)
            (32, 2),  // 32 / 16 == 2, next_power_of_two(2) == 2
            (48, 4),  // 48 / 16 == 3, next_power_of_two(3) == 4
            (64, 4),  // 64 / 16 == 4, next_power_of_two(4) == 4
            (100, 8), // 100 / 16 == 6, next_power_of_two(6) == 8
            (128, 8), // 128 / 16 == 8, next_power_of_two(8) == 8
        ];
        for (n, expected) in cases {
            let c = ShardedLruCache::<u32, u32>::builder()
                .max_size(n)
                .build()
                .unwrap();
            assert_eq!(c.shards(), expected, "max_size={n}");
            // `new(n)` must route through the identical default path.
            assert_eq!(
                ShardedLruCache::<u32, u32>::new(n).shards(),
                expected,
                "max_size={n}"
            );
        }
    }

    #[test]
    fn default_shard_count_clamps_at_upper_bound_end_to_end() {
        // The high-core-host interaction the builder tests otherwise never reach: a max_size large
        // enough that the capacity-derived count would exceed the CPU-derived ceiling must clamp
        // back to exactly default_shard_count(). Expectation is computed at runtime, so this holds
        // on an 8-core box and a 256-core box alike.
        use crate::stores::sharded::default_shard_count;
        let d = default_shard_count();
        let big = d.checked_mul(16).unwrap().checked_mul(4).unwrap();
        let c = ShardedLruCache::<u32, u32>::builder()
            .max_size(big)
            .build()
            .unwrap();
        assert_eq!(
            c.shards(),
            d,
            "derived count must clamp down to default_shard_count()"
        );
    }

    #[test]
    fn deep_clone_preserves_capacity_derived_shard_count() {
        // deep_clone copies the source's actual shard array length; it must NOT re-derive the
        // count from capacity. A source built on the scaled-down default path (8 shards for
        // max_size 100) must clone to 8 shards, not back up to the plain default.
        let src = ShardedLruCache::<u32, u32>::builder()
            .max_size(100)
            .build()
            .unwrap();
        let derived = src.shards();
        let clone = src.deep_clone();
        assert_eq!(clone.shards(), derived);
        assert_eq!(clone.capacity(), src.capacity());
    }

    #[test]
    fn copy_from_derives_shard_count_from_new_builder_not_source() {
        // copy_from builds a fresh cache from the NEW builder, so the destination shard count
        // follows the new builder's own default/explicit resolution -- never inherited from the
        // source. Entries survive the re-sharding.
        use crate::stores::sharded::default_shard_count_for_capacity;
        let src = ShardedLruCache::<u32, u32>::builder()
            .shards(64)
            .max_size(4096)
            .build()
            .unwrap();
        assert_eq!(src.shards(), 64);
        src.set(1, 10);
        src.set(2, 20);

        // Default-path destination with a small max_size derives a scaled-down count (8), not 64.
        let dst = ShardedLruCache::<u32, u32>::builder()
            .max_size(100)
            .copy_from(&src)
            .unwrap();
        assert_eq!(dst.shards(), default_shard_count_for_capacity(Some(100)));
        assert_eq!(dst.get(&1), Some(10));
        assert_eq!(dst.get(&2), Some(20));

        // Explicit shards on the destination stay authoritative through copy_from.
        let dst2 = ShardedLruCache::<u32, u32>::builder()
            .shards(32)
            .max_size(4096)
            .copy_from(&src)
            .unwrap();
        assert_eq!(dst2.shards(), 32);
        assert_eq!(dst2.get(&1), Some(10));
    }

    #[test]
    #[should_panic(expected = "non-zero max_size")]
    fn new_zero_max_size_panics() {
        let _c = ShardedLruCache::<u32, u32>::new(0);
    }

    #[test]
    fn basic_get_set_remove() {
        let c = ShardedLruCache::<u32, u32>::builder()
            .max_size(64)
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
        let c1 = ShardedLruCache::<u32, u32>::builder()
            .max_size(64)
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
    fn eviction_fires() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let count = Arc::new(AtomicUsize::new(0));
        let count2 = count.clone();
        let c = ShardedLruCache::<u32, u32>::builder()
            .max_size(8)
            .shards(1) // single shard so capacity=8 exactly
            .on_evict(move |_, _| {
                count2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();
        for i in 0..16u32 {
            SyncConcurrentCached::cache_set(&c, i, i).expect("insert must succeed");
        }
        assert!(
            count.load(Ordering::Relaxed) > 0,
            "eviction should have fired"
        );
    }

    #[test]
    fn cache_remove_fires_on_evict() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let count = Arc::new(AtomicUsize::new(0));
        let count2 = count.clone();
        let c = ShardedLruCache::<u32, u32>::builder()
            .max_size(64)
            .shards(1)
            .on_evict(move |_, _| {
                count2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1, 10).expect("insert must succeed");
        SyncConcurrentCached::cache_remove(&c, &1).expect("key must be present");
        assert_eq!(
            count.load(Ordering::Relaxed),
            1,
            "on_evict must fire on successful cache_remove"
        );
    }

    #[test]
    fn cache_remove_increments_eviction_metrics() {
        let c = ShardedLruCache::<u32, u32>::builder()
            .max_size(64)
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1, 10).expect("insert must succeed");
        SyncConcurrentCached::cache_set(&c, 2, 20).expect("insert must succeed");
        let before = c
            .metrics()
            .evictions
            .expect("eviction-tracking stores report an evictions count");
        SyncConcurrentCached::cache_remove(&c, &1).expect("key must be present");
        SyncConcurrentCached::cache_remove(&c, &999).expect("cache_remove must succeed");
        let after = c
            .metrics()
            .evictions
            .expect("eviction-tracking stores report an evictions count");
        assert_eq!(
            after - before,
            1,
            "successful remove must increment evictions"
        );
    }

    #[test]
    fn per_shard_max_size_and_size_exclusive() {
        let err = ShardedLruCache::<u32, u32>::builder()
            .max_size(100)
            .per_shard_max_size(10)
            .build();
        assert!(err.is_err());
    }

    #[test]
    fn build_rejects_overflowing_shards_and_capacity() {
        let err = ShardedLruCache::<u32, u32>::builder()
            .max_size(1)
            .shards(usize::MAX)
            .build();
        assert!(matches!(
            err,
            Err(BuildError::InvalidValue {
                field: "shards",
                ..
            })
        ));

        let err = ShardedLruCache::<u32, u32>::builder()
            .per_shard_max_size(usize::MAX)
            .shards(2)
            .build();
        assert!(matches!(
            err,
            Err(BuildError::InvalidValue {
                field: "per_shard_max_size",
                ..
            })
        ));
    }

    #[test]
    fn copy_from_preserves_entries() {
        // Use shards(1) to avoid per-shard capacity eviction during insertion.
        let old = ShardedLruCache::<u32, u32>::builder()
            .max_size(1024)
            .shards(1)
            .build()
            .unwrap();
        for i in 0..50u32 {
            SyncConcurrentCached::cache_set(&old, i, i * 10).expect("insert must succeed");
        }
        let new_cache = ShardedLruCache::<u32, u32>::builder()
            .max_size(1024)
            .shards(4)
            .copy_from(&old)
            .unwrap();
        for i in 0..50u32 {
            assert_eq!(
                SyncConcurrentCached::cache_get(&new_cache, &i).expect("key was just inserted"),
                Some(i * 10)
            );
        }
    }

    #[test]
    fn copy_from_respects_capacity() {
        let old = ShardedLruCache::<u32, u32>::builder()
            .max_size(64)
            .shards(1)
            .build()
            .unwrap();
        for i in 0..32u32 {
            SyncConcurrentCached::cache_set(&old, i, i).expect("insert must succeed");
        }
        // new cache has smaller capacity
        let new_cache = ShardedLruCache::<u32, u32>::builder()
            .max_size(16)
            .shards(1)
            .copy_from(&old)
            .unwrap();
        assert!(new_cache.len() <= 16);
    }

    #[test]
    fn builder_error_context() {
        let err = ShardedLruCache::<u32, u32>::builder()
            .max_size(0)
            .build()
            .expect_err("zero size should be an error");
        let message = err.to_string();
        assert!(
            message.contains("max_size"),
            "error should mention max_size"
        );

        let err = ShardedLruCache::<u32, u32>::builder()
            .max_size(1)
            .shards(0)
            .build()
            .expect_err("zero shards should be an error");
        let message = err.to_string();
        assert!(message.contains("shards"), "error should mention shards");
    }

    #[test]
    fn send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ShardedLruCache<u32, u32>>();
    }

    #[test]
    fn cache_clear_with_on_evict_fires_for_all_entries() {
        use std::sync::atomic::{AtomicU64, Ordering};
        let count = Arc::new(AtomicU64::new(0));
        let count2 = count.clone();
        let c = ShardedLruCache::<u32, u32>::builder()
            .shards(1)
            .max_size(64)
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
        // metrics().evictions must not depend on whether an on_evict observer is attached:
        // cache_clear_with_on_evict counts every removed entry even with no callback.
        let c = ShardedLruCache::<u32, u32>::builder()
            .shards(1)
            .max_size(64)
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
        // Plain clear() stays silent (no eviction counting).
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
        let c = ShardedLruCache::<u32, u32>::builder()
            .max_size(64)
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
    fn cache_remove_entry_basic() {
        let c = ShardedLruCache::<u32, u32>::builder()
            .shards(1)
            .max_size(8)
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
    fn cache_remove_entry_fires_on_evict() {
        use std::sync::atomic::{AtomicU64, Ordering};
        let count = Arc::new(AtomicU64::new(0));
        let count2 = count.clone();
        let c = ShardedLruCache::<u32, u32>::builder()
            .shards(1)
            .max_size(8)
            .on_evict(move |_, _| {
                count2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1u32, 10u32).expect("insert must succeed");
        SyncConcurrentCached::cache_remove_entry(&c, &1u32).expect("key must be present");
        assert_eq!(count.load(Ordering::Relaxed), 1);

        SyncConcurrentCached::cache_remove_entry(&c, &999u32)
            .expect("cache_remove_entry must succeed");
        assert_eq!(count.load(Ordering::Relaxed), 1, "no fire for absent key");
    }

    #[test]
    fn cache_remove_entry_increments_eviction_counter() {
        let c = ShardedLruCache::<u32, u32>::builder()
            .shards(1)
            .max_size(8)
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1u32, 10u32).expect("insert must succeed");
        let before = c.metrics().evictions.expect("evictions are always tracked");
        SyncConcurrentCached::cache_remove_entry(&c, &1u32).expect("key must be present");
        SyncConcurrentCached::cache_remove_entry(&c, &999u32)
            .expect("cache_remove_entry must succeed"); // absent — must not increment
        assert_eq!(
            c.metrics().evictions.expect("evictions are always tracked") - before,
            1,
            "cache_remove_entry must increment evictions for present key only"
        );
    }

    #[test]
    fn cache_delete_returns_true_for_present_entry() {
        let c = ShardedLruCache::<u32, u32>::builder()
            .shards(1)
            .max_size(8)
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1u32, 10u32).expect("insert must succeed");
        assert!(SyncConcurrentCached::cache_delete(&c, &1u32).expect("cache_delete must succeed"));
        assert!(!SyncConcurrentCached::cache_delete(&c, &1u32).expect("cache_delete must succeed"));
    }

    // --- Inherent infallible method tests ---

    #[test]
    fn inherent_get_returns_option_not_result() {
        let c = ShardedLruCache::<u32, u32>::builder()
            .max_size(64)
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
        let c = ShardedLruCache::<u32, u32>::builder()
            .max_size(64)
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
        let c = ShardedLruCache::<u32, u32>::builder()
            .max_size(64)
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
        let c = ShardedLruCache::<u32, u32>::builder()
            .shards(1)
            .max_size(64)
            .build()
            .unwrap();
        c.set(7, 77);
        let pair: Option<(u32, u32)> = c.remove_entry(&7);
        assert_eq!(pair, Some((7, 77)));
        assert_eq!(c.remove_entry(&7), None);
    }

    #[test]
    fn inherent_delete_returns_bool() {
        let c = ShardedLruCache::<u32, u32>::builder()
            .max_size(64)
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
        let c = ShardedLruCache::<u32, u32>::builder()
            .max_size(64)
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
    fn retain_preserves_survivor_recency_order() {
        // shards(1) so the recency order is deterministic and observable from one shard.
        let c = ShardedLruCache::<u32, u32>::builder()
            .shards(1)
            .max_size(64)
            .build()
            .unwrap();
        for i in 0..10u32 {
            SyncConcurrentCached::cache_set(&c, i, i).expect("insert must succeed");
        }
        let before: Vec<u32> = c.inner.shards[0]
            .lock
            .read()
            .iter_order_raw()
            .into_iter()
            .map(|(k, _)| k)
            .filter(|k| k % 2 == 0)
            .collect();

        c.retain(|k, _| k % 2 == 0);

        let after: Vec<u32> = c.inner.shards[0]
            .lock
            .read()
            .iter_order_raw()
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert_eq!(
            before, after,
            "surviving entries must keep their relative MRU order"
        );
    }

    /// `cache_clear_with_on_evict` drains each shard with `LruCache::drain_all` (no key
    /// clones, no re-hashing) instead of "collect every key, then `pop_raw` each one". The
    /// firing order is load-bearing: `drain_all` walks the LRU chain, so callbacks must still
    /// arrive most-recently-used first, per shard.
    #[test]
    fn cache_clear_with_on_evict_fires_mru_to_lru_per_shard() {
        use std::sync::Mutex;
        let seen: Arc<Mutex<Vec<u32>>> = Arc::new(Mutex::new(Vec::new()));
        let seen2 = Arc::clone(&seen);
        let c = ShardedLruCache::<u32, u32>::builder()
            .shards(1) // one shard: a single, fully deterministic recency chain
            .max_size(64)
            .on_evict(move |k: &u32, _v: &u32| seen2.lock().unwrap().push(*k))
            .build()
            .unwrap();
        for i in 0..6u32 {
            SyncConcurrentCached::cache_set(&c, i, i).expect("insert must succeed");
        }
        // Re-read 0 and 2 so the recency chain is not simply insertion order reversed.
        assert_eq!(SyncConcurrentCached::cache_get(&c, &0).unwrap(), Some(0));
        assert_eq!(SyncConcurrentCached::cache_get(&c, &2).unwrap(), Some(2));
        let expected: Vec<u32> = c.inner.shards[0]
            .lock
            .read()
            .iter_order_raw()
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert_eq!(
            expected,
            vec![2, 0, 5, 4, 3, 1],
            "precondition: MRU -> LRU chain after the two re-reads"
        );

        c.cache_clear_with_on_evict();

        assert_eq!(
            *seen.lock().unwrap(),
            expected,
            "on_evict must fire in MRU -> LRU order"
        );
        assert!(c.is_empty(), "every entry must be drained");
        // The drained shard is immediately reusable (drain_all resets the slab sentinels).
        SyncConcurrentCached::cache_set(&c, 42, 42).expect("insert must succeed");
        assert_eq!(SyncConcurrentCached::cache_get(&c, &42).unwrap(), Some(42));
        assert_eq!(c.len(), 1);
    }

    /// `cache_get` clones the value and releases the shard lock before bumping the
    /// hit/miss counters; the counters must still be exact for both outcomes, and a hit
    /// must still promote LRU recency.
    #[test]
    fn cache_get_counts_and_promotes_after_releasing_the_lock() {
        let c = ShardedLruCache::<u32, u32>::builder()
            .shards(1)
            .max_size(2)
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1, 10).expect("insert must succeed");
        SyncConcurrentCached::cache_set(&c, 2, 20).expect("insert must succeed");
        assert_eq!(SyncConcurrentCached::cache_get(&c, &1).unwrap(), Some(10));
        assert_eq!(SyncConcurrentCached::cache_get(&c, &99).unwrap(), None);
        let m = c.metrics();
        assert_eq!(m.hits, Some(1));
        assert_eq!(m.misses, Some(1));
        // The hit promoted key 1, so the capacity eviction must claim key 2.
        SyncConcurrentCached::cache_set(&c, 3, 30).expect("insert must succeed");
        assert!(
            SyncConcurrentCached::cache_contains(&c, &1).unwrap(),
            "the entry read via cache_get must have been promoted to MRU"
        );
        assert!(!SyncConcurrentCached::cache_contains(&c, &2).unwrap());
    }

    /// Counter-wiring contract: `retain`'s eviction count on the plain LRU sharded store
    /// is the per-shard `LruCache::evictions` counter directly (there is no separate
    /// outer/inner split here, unlike the expiry-aware sharded stores).
    #[test]
    fn retain_increments_per_shard_evictions_counter_directly() {
        let c = ShardedLruCache::<u32, u32>::builder()
            .shards(1)
            .max_size(64)
            .build()
            .unwrap();
        for i in 0..10u32 {
            SyncConcurrentCached::cache_set(&c, i, i).expect("insert must succeed");
        }
        let before = c.inner.shards[0]
            .lock
            .read()
            .evictions
            .load(Ordering::Relaxed);
        c.retain(|k, _| k % 2 == 0);
        let after = c.inner.shards[0]
            .lock
            .read()
            .evictions
            .load(Ordering::Relaxed);
        assert_eq!(
            after - before,
            5,
            "retain must bump the per-shard LRU evictions counter by exactly the removed count"
        );
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
        let c = ShardedLruCache::<u32, u32>::builder()
            .max_size(64)
            .build()
            .unwrap();
        use_trait(&c, 1, 100);
    }
}
