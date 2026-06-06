use std::hash::Hash;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(feature = "async_core")]
use crate::ConcurrentCachedAsync;
use crate::{CacheMetrics, CachedIter, ConcurrentCached};

use super::{
    CachePadded, DefaultShardHasher, Shard, ShardHasher, checked_shard_count, shard_index,
};
use crate::stores::{BuildError, LruCache};

type OnEvict<K, V> = Arc<dyn Fn(&K, &V) + Send + Sync>;

#[allow(clippy::type_complexity)]
struct LruInner<K, V, H> {
    shards: Box<[CachePadded<Shard<LruCache<K, V>>>]>,
    shard_mask: usize,
    hasher: H,
    on_evict: Option<OnEvict<K, V>>,
    /// Total logical capacity (sum of per-shard caps).
    total_capacity: usize,
}

/// A fully-concurrent, partitioned, LRU-bounded in-memory cache.
///
/// Wraps an `Arc` — `clone()` is an Arc-share (shared state), not a deep copy.
/// Use [`deep_clone`](ShardedLruCacheBase::deep_clone) to get an independent copy.
///
/// This is a type alias for `ShardedLruCacheBase<K, V, DefaultShardHasher>`.
/// To use a custom shard hasher, construct a [`ShardedLruCacheBase`] directly via
/// [`ShardedLruCacheBase::builder()`].
///
/// **Note**: LRU promotion requires mutable access to the per-shard store, so
/// `cache_get` acquires a **write** lock (unlike `ShardedCache` which only needs a read lock).
/// Under many concurrent readers this can be a bottleneck; consider `ShardedCache` if you do
/// not need capacity bounding.
///
/// **Note**: `K` must implement `Clone` (needed for LRU key tracking). `ShardedCache<K, V>`
/// requires only `K: Hash + Eq`. `V` must also implement `Clone`, because reads return owned
/// values cloned from under the shard lock.
///
/// **Note**: Setting an `on_evict` callback requires the callback itself to be `'static` because
/// the cache stores it behind an `Arc<dyn Fn(&K, &V) + Send + Sync>`. This does not add `'static`
/// bounds to `K` or `V`.
pub type ShardedLruCache<K, V> = ShardedLruCacheBase<K, V, DefaultShardHasher>;

/// Backing type for [`ShardedLruCache`] with a generic shard hasher `H`.
pub struct ShardedLruCacheBase<K, V, H = DefaultShardHasher> {
    inner: Arc<LruInner<K, V, H>>,
}

impl<K, V, H> Clone for ShardedLruCacheBase<K, V, H> {
    /// Arc-share clone — both handles point to the same underlying cache.
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<K, V, H> std::fmt::Debug for ShardedLruCacheBase<K, V, H> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShardedLruCache")
            .field("shards", &self.inner.shards.len())
            .field("capacity", &self.inner.total_capacity)
            .finish_non_exhaustive()
    }
}

impl<K, V, H> ShardedLruCacheBase<K, V, H>
where
    K: Hash + Eq + Clone,
    H: ShardHasher<K>,
{
    /// Return a builder for constructing a [`ShardedLruCacheBase`].
    ///
    /// Always returns a builder with the [`DefaultShardHasher`], regardless of the `H` type
    /// parameter on `Self`. Call `.hasher(h)` on the builder to use a custom hasher.
    pub fn builder() -> ShardedLruCacheBuilder<K, V, DefaultShardHasher> {
        ShardedLruCacheBuilder::default()
    }

    #[inline]
    fn shard_of(&self, k: &K) -> &CachePadded<Shard<LruCache<K, V>>> {
        let h = self.inner.hasher.shard_hash(k);
        &self.inner.shards[shard_index(h, self.inner.shard_mask)]
    }
}

impl<K: Clone + Hash + Eq, V: Clone, H: ShardHasher<K> + Clone> ShardedLruCacheBase<K, V, H> {
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
                total_capacity: self.inner.total_capacity,
            }),
        }
    }
}

impl<K, V, H: ShardHasher<K>> ShardedLruCacheBase<K, V, H>
where
    K: Hash + Eq + Clone,
{
    /// Return aggregate metrics across all shards.
    ///
    /// `evictions` counts both LRU capacity evictions (tracked per-shard) and
    /// explicit removes via [`ConcurrentCached::cache_remove`].
    /// `capacity` reflects the effective total capacity — may exceed the requested
    /// `size` when the 16-per-shard minimum floor is applied; see [`capacity`](Self::capacity).
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
            size,
            capacity: Some(self.inner.total_capacity),
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
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner
            .shards
            .iter()
            .map(|s| s.lock.read().cache_size())
            .sum()
    }

    /// `true` if no entries are present.
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
    /// If no `on_evict` callback is configured, this is equivalent to [`clear`](Self::clear).
    /// Increments the evictions counter for each removed entry only when `on_evict` is set.
    pub fn cache_clear_with_on_evict(&self) {
        if self.inner.on_evict.is_none() {
            return self.clear();
        }
        for shard in self.inner.shards.iter() {
            let removed: Vec<(K, V)> = {
                let mut guard = shard.lock.write();
                let keys: Vec<K> = guard.iter().map(|(k, _)| k.clone()).collect();
                let mut removed = Vec::with_capacity(keys.len());
                for k in keys {
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
            if let Some(on_evict) = &self.inner.on_evict {
                for (k, v) in &removed {
                    on_evict(k, v);
                }
            }
        }
    }

    /// Effective total capacity across all shards.
    ///
    /// When constructed with [`max_size`](ShardedLruCacheBuilder::max_size), this may
    /// be larger than the requested size because per-shard capacity is rounded
    /// up with ceiling division.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.inner.total_capacity
    }
}

use crate::Cached;

impl<K, V, H> ConcurrentCached<K, V> for ShardedLruCacheBase<K, V, H>
where
    K: Hash + Eq + Clone,
    V: Clone,
    H: ShardHasher<K>,
{
    type Error = std::convert::Infallible;

    fn cache_get(&self, k: &K) -> Result<Option<V>, Self::Error> {
        let shard = self.shard_of(k);
        let mut guard = shard.lock.write();
        match guard.cache_get(k) {
            Some(v) => {
                let v = v.clone();
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
        if let Some((ref key, ref value)) = removed {
            if let Some(on_evict) = &self.inner.on_evict {
                on_evict(key, value);
            }
        }
        Ok(removed)
    }

    fn cache_size(&self) -> Result<Option<usize>, Self::Error> {
        Ok(Some(self.len()))
    }

    /// No-op: this store has no TTL to refresh on hit. Always returns `false`.
    fn set_refresh_on_hit(&self, _refresh: bool) -> bool {
        false
    }
}

#[cfg(feature = "async_core")]
impl<K, V, H> ConcurrentCachedAsync<K, V> for ShardedLruCacheBase<K, V, H>
where
    K: Hash + Eq + Clone + Send + Sync,
    V: Clone + Send + Sync,
    H: ShardHasher<K>,
{
    type Error = std::convert::Infallible;

    async fn cache_get(&self, k: &K) -> Result<Option<V>, Self::Error> {
        ConcurrentCached::cache_get(self, k)
    }

    async fn cache_set(&self, k: K, v: V) -> Result<Option<V>, Self::Error> {
        ConcurrentCached::cache_set(self, k, v)
    }

    async fn cache_remove(&self, k: &K) -> Result<Option<V>, Self::Error> {
        ConcurrentCached::cache_remove(self, k)
    }

    async fn cache_remove_entry(&self, k: &K) -> Result<Option<(K, V)>, Self::Error> {
        ConcurrentCached::cache_remove_entry(self, k)
    }

    fn cache_size(&self) -> Result<Option<usize>, Self::Error> {
        Ok(Some(self.len()))
    }

    fn set_refresh_on_hit(&self, b: bool) -> bool {
        <Self as ConcurrentCached<K, V>>::set_refresh_on_hit(self, b)
    }
}

/// Builder for [`ShardedLruCacheBase`].
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

impl<K, V, H> ShardedLruCacheBuilder<K, V, H> {
    /// Set the requested total capacity (divided across shards via `div_ceil`).
    ///
    /// Eviction is enforced independently per shard. Each shard gets
    /// `ceil(size / shards)` entries, with a minimum of 16 per shard when
    /// `shards > 1` to avoid capacity fragmentation/eviction flakes.
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

    /// Set a callback invoked when an entry is evicted by LRU capacity pressure, explicit
    /// [`cache_remove`](ConcurrentCached::cache_remove), or
    /// [`cache_remove_entry`](ConcurrentCached::cache_remove_entry).
    /// Does **not** fire on [`clear`](ShardedLruCacheBase::clear);
    /// use [`cache_clear_with_on_evict`](ShardedLruCacheBase::cache_clear_with_on_evict) to opt in.
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

    /// Build the cache, returning an error if required fields are missing or invalid.
    ///
    /// Use [`ShardedLruCache::builder()`] (or [`ShardedLruCacheBase::builder()`]) to obtain
    /// a builder, set at least [`max_size`](Self::max_size) or
    /// [`per_shard_max_size`](Self::per_shard_max_size), then call `.build()`.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError`] if `max_size` (or `per_shard_max_size`) was not set, is `0`,
    /// or if both `max_size` and `per_shard_max_size` are set simultaneously, or if the
    /// effective sharded capacity overflows `usize`.
    pub fn build(self) -> Result<ShardedLruCacheBase<K, V, H>, BuildError>
    where
        K: Hash + Eq + Clone,
        H: ShardHasher<K>,
    {
        let n = checked_shard_count(self.shards)?;
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
        Ok(ShardedLruCacheBase {
            inner: Arc::new(LruInner {
                shards,
                shard_mask: mask,
                hasher: self
                    .hasher
                    .expect("hasher is always initialized via Default or .hasher()"),
                on_evict: self.on_evict,
                total_capacity: total_cap,
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
    #[must_use]
    pub fn copy_from<H2: ShardHasher<K>>(
        self,
        existing: &ShardedLruCacheBase<K, V, H2>,
    ) -> ShardedLruCacheBase<K, V, H>
    where
        K: Clone + Hash + Eq,
        V: Clone,
        H: ShardHasher<K>,
    {
        let new_cache = self
            .build()
            .unwrap_or_else(|e| panic!("ShardedLruCache build failed: {e}"));
        for shard in existing.inner.shards.iter() {
            // iter_order returns MRU-first; insert in reverse (LRU-first)
            // so that the MRU entries are pushed in last and land at the head.
            let entries: Vec<(K, V)> = {
                let guard = shard.lock.read();
                guard.iter_order()
            };
            for (k, v) in entries.into_iter().rev() {
                let _ = ConcurrentCached::cache_set(&new_cache, k, v);
            }
        }
        new_cache
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ConcurrentCached as SyncConcurrentCached;

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
        let c = ShardedLruCacheBase::<u32, u32>::builder()
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
        let c = ShardedLruCacheBase::<u32, u32>::builder()
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
        let err = ShardedLruCacheBase::<u32, u32>::builder()
            .max_size(100)
            .per_shard_max_size(10)
            .build();
        assert!(err.is_err());
    }

    #[test]
    fn build_rejects_overflowing_shards_and_capacity() {
        let err = ShardedLruCacheBase::<u32, u32>::builder()
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

        let err = ShardedLruCacheBase::<u32, u32>::builder()
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
        let old = ShardedLruCacheBase::<u32, u32>::builder()
            .max_size(1024)
            .shards(1)
            .build()
            .unwrap();
        for i in 0..50u32 {
            SyncConcurrentCached::cache_set(&old, i, i * 10).expect("insert must succeed");
        }
        let new_cache = ShardedLruCacheBase::<u32, u32>::builder()
            .max_size(1024)
            .shards(4)
            .copy_from(&old);
        for i in 0..50u32 {
            assert_eq!(
                SyncConcurrentCached::cache_get(&new_cache, &i).expect("key was just inserted"),
                Some(i * 10)
            );
        }
    }

    #[test]
    fn copy_from_respects_capacity() {
        let old = ShardedLruCacheBase::<u32, u32>::builder()
            .max_size(64)
            .shards(1)
            .build()
            .unwrap();
        for i in 0..32u32 {
            SyncConcurrentCached::cache_set(&old, i, i).expect("insert must succeed");
        }
        // new cache has smaller capacity
        let new_cache = ShardedLruCacheBase::<u32, u32>::builder()
            .max_size(16)
            .shards(1)
            .copy_from(&old);
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
        let c = ShardedLruCacheBase::<u32, u32>::builder()
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
    fn clear_does_not_fire_on_evict() {
        use std::sync::atomic::{AtomicU64, Ordering};
        let count = Arc::new(AtomicU64::new(0));
        let count2 = count.clone();
        let c = ShardedLruCacheBase::<u32, u32>::builder()
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
        let c = ShardedLruCacheBase::<u32, u32>::builder()
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
        let c = ShardedLruCacheBase::<u32, u32>::builder()
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
        let c = ShardedLruCacheBase::<u32, u32>::builder()
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
        let c = ShardedLruCacheBase::<u32, u32>::builder()
            .shards(1)
            .max_size(8)
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1u32, 10u32).expect("insert must succeed");
        assert!(SyncConcurrentCached::cache_delete(&c, &1u32).expect("cache_delete must succeed"));
        assert!(!SyncConcurrentCached::cache_delete(&c, &1u32).expect("cache_delete must succeed"));
    }
}
