use std::hash::Hash;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(feature = "ahash")]
use ahash::RandomState;
#[cfg(not(feature = "ahash"))]
use std::collections::hash_map::RandomState;

use std::collections::HashMap;

#[cfg(feature = "async_core")]
use crate::ConcurrentCachedAsync;
use crate::{CacheMetrics, ConcurrentCacheBase, ConcurrentCached};

use super::{
    CachePadded, DefaultShardHasher, Shard, ShardHasher, checked_shard_count, shard_index,
};
use crate::stores::BuildError;

type OnEvict<K, V> = Arc<dyn Fn(&K, &V) + Send + Sync>;

#[allow(clippy::type_complexity)]
struct UnboundInner<K, V, H> {
    shards: Box<[CachePadded<Shard<HashMap<K, V, RandomState>>>]>,
    shard_mask: usize,
    hasher: H,
    on_evict: Option<OnEvict<K, V>>,
}

/// A fully-concurrent, partitioned, unbounded in-memory cache.
///
/// Wraps an `Arc` — `clone()` is an Arc-share (shared state), not a deep copy.
/// Use [`deep_clone`](ShardedUnboundCacheBase::deep_clone) to get an independent copy.
///
/// **Note**: reads return owned values cloned from under the shard lock, so `V` must
/// implement `Clone`.
///
/// This is a type alias for `ShardedUnboundCacheBase<K, V, DefaultShardHasher>`.
/// To use a custom shard hasher, call [`ShardedUnboundCache::builder()`] and then
/// [`hasher`](ShardedUnboundCacheBuilder::hasher), which yields a `ShardedUnboundCacheBase<K, V, H>`
/// over your hasher.
pub type ShardedUnboundCache<K, V> = ShardedUnboundCacheBase<K, V, DefaultShardHasher>;

/// Backing type for [`ShardedUnboundCache`] with a generic shard hasher `H`.
///
/// In most cases prefer the [`ShardedUnboundCache`] alias which uses the default
/// shard hasher (ahash-backed when the `ahash` feature is enabled, otherwise
/// `std::collections::hash_map::RandomState`). Use this type directly only
/// when you need a custom [`ShardHasher`] implementation.
pub struct ShardedUnboundCacheBase<K, V, H = DefaultShardHasher> {
    inner: Arc<UnboundInner<K, V, H>>,
}

impl<K, V, H> Clone for ShardedUnboundCacheBase<K, V, H> {
    /// Arc-share clone — both handles point to the same underlying cache.
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<K, V, H> std::fmt::Debug for ShardedUnboundCacheBase<K, V, H> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShardedUnboundCache")
            .field("shards", &self.inner.shards.len())
            .finish_non_exhaustive()
    }
}

impl<K, V> ShardedUnboundCacheBase<K, V, DefaultShardHasher>
where
    K: Hash + Eq,
{
    /// Construct a ready-to-use [`ShardedUnboundCache`] with the [`DefaultShardHasher`] and a
    /// default shard count.
    ///
    /// `ShardedUnboundCache` has no required configuration, so this never fails. For a custom
    /// hasher, shard count, or `on_evict`, use [`builder`](Self::builder).
    #[must_use]
    pub fn new() -> ShardedUnboundCache<K, V> {
        Self::builder()
            .build()
            .expect("ShardedUnboundCache default build is infallible")
    }

    /// Return a builder for constructing a [`ShardedUnboundCache`].
    ///
    /// The builder starts with the [`DefaultShardHasher`]. To use a custom hasher, call
    /// [`hasher`](ShardedUnboundCacheBuilder::hasher) on the returned builder; it switches the
    /// builder's hasher type and `build` then yields a `ShardedUnboundCacheBase` over that hasher.
    /// `new` and `builder` exist only on the default-hasher alias, so a custom hasher is always
    /// introduced via `hasher`, never a `ShardedUnboundCacheBase::<_, _, H>` turbofish.
    #[must_use]
    pub fn builder() -> ShardedUnboundCacheBuilder<K, V, DefaultShardHasher> {
        ShardedUnboundCacheBuilder::default()
    }
}

impl<K, V, H> ShardedUnboundCacheBase<K, V, H>
where
    K: Hash + Eq,
    H: ShardHasher<K>,
{
    #[inline]
    fn shard_of(&self, k: &K) -> &CachePadded<Shard<HashMap<K, V, RandomState>>> {
        let h = self.inner.hasher.shard_hash(k);
        &self.inner.shards[shard_index(h, self.inner.shard_mask)]
    }
}

impl<K, V> Default for ShardedUnboundCache<K, V>
where
    K: Hash + Eq,
{
    fn default() -> Self {
        ShardedUnboundCacheBuilder::default()
            .build()
            .unwrap_or_else(|e| panic!("ShardedUnboundCache build failed: {e}"))
    }
}

impl<K: Clone + Hash + Eq, V: Clone, H: ShardHasher<K>> ShardedUnboundCacheBase<K, V, H> {
    /// Return an independent deep copy of this cache — entries and metrics are
    /// duplicated, not shared. In most cases [`Clone::clone`] (Arc-share) is
    /// what you want.
    ///
    /// ```rust
    /// use cached::ShardedUnboundCache;
    ///
    /// let cache: ShardedUnboundCache<String, u32> = ShardedUnboundCache::new();
    /// cache.set("k".to_string(), 1);
    ///
    /// let shared = cache.clone();     // Arc clone — same backing store
    /// let deep   = cache.deep_clone(); // independent snapshot
    ///
    /// cache.set("k".to_string(), 2);
    /// assert_eq!(shared.get(&"k".to_string()), Some(2)); // sees update
    /// assert_eq!(deep.get(&"k".to_string()),   Some(1)); // snapshot unchanged
    /// ```
    #[must_use]
    pub fn deep_clone(&self) -> Self {
        let n = self.inner.shards.len();
        let shards = (0..n)
            .map(|i| {
                let guard = self.inner.shards[i].lock.read();
                let store_copy = guard.clone();
                // Load the hit/miss atomics while still holding the shard read
                // lock, matching ShardedLruCache::deep_clone (src/stores/sharded/
                // lru.rs): dropping the guard first would let a concurrent writer
                // mutate the entries and bump the counters in between, pairing a
                // stale entry snapshot with newer metrics (C7).
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
            inner: Arc::new(UnboundInner {
                shards,
                shard_mask: self.inner.shard_mask,
                hasher: self.inner.hasher.clone(),
                on_evict: self.inner.on_evict.clone(),
            }),
        }
    }
}

impl<K, V, H: ShardHasher<K>> ShardedUnboundCacheBase<K, V, H>
where
    K: Hash + Eq,
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
    /// This is the infallible ergonomic API for the concrete type.
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
}

impl<K, V, H: ShardHasher<K>> ShardedUnboundCacheBase<K, V, H>
where
    K: Hash + Eq,
{
    /// Return aggregate metrics across all shards.
    ///
    /// Note: the returned value is approximate under concurrent mutation — no global lock is held
    /// across shards; each shard is locked and read one at a time.
    #[must_use]
    pub fn metrics(&self) -> CacheMetrics {
        let mut hits = 0u64;
        let mut misses = 0u64;
        let mut size = 0usize;
        for shard in self.inner.shards.iter() {
            hits += shard.hits.load(Ordering::Relaxed);
            misses += shard.misses.load(Ordering::Relaxed);
            size += shard.lock.read().len();
        }
        CacheMetrics {
            hits: Some(hits),
            misses: Some(misses),
            evictions: None,
            entry_count: Some(size),
            capacity: None,
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
            .map(|s| s.lock.read().len())
            .collect()
    }

    /// Total number of live entries across all shards.
    ///
    /// Note: the returned value is approximate under concurrent mutation — no global lock is held
    /// across shards; each shard is locked and read one at a time.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.shards.iter().map(|s| s.lock.read().len()).sum()
    }

    /// `true` if no entries are present.
    ///
    /// Note: the returned value is approximate under concurrent mutation — no global lock is held
    /// across shards; each shard is locked and read one at a time.
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
    /// If no `on_evict` callback is configured, this is equivalent to [`clear`](Self::clear).
    ///
    /// **Note:** `ShardedUnboundCache` does not track eviction counts — `metrics().evictions` always
    /// returns `None` regardless of whether `on_evict` fires. This differs from the
    /// eviction-tracking sharded stores, whose `cache_clear_with_on_evict` always counts the
    /// removed entries as evictions; the unbounded store has no eviction counter to increment.
    pub fn cache_clear_with_on_evict(&self) {
        if self.inner.on_evict.is_none() {
            return self.clear();
        }
        for shard in self.inner.shards.iter() {
            let entries: Vec<(K, V)> = shard.lock.write().drain().collect();
            if let Some(on_evict) = &self.inner.on_evict {
                for (k, v) in &entries {
                    on_evict(k, v);
                }
            }
        }
    }
}

impl<K, V, H> ConcurrentCacheBase for ShardedUnboundCacheBase<K, V, H>
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
}

impl<K, V, H> ConcurrentCached<K, V> for ShardedUnboundCacheBase<K, V, H>
where
    K: Hash + Eq,
    V: Clone,
    H: ShardHasher<K>,
{
    fn cache_get(&self, k: &K) -> Result<Option<V>, Self::Error> {
        let shard = self.shard_of(k);
        let guard = shard.lock.read();
        match guard.get(k) {
            Some(v) => {
                shard.hits.fetch_add(1, Ordering::Relaxed);
                Ok(Some(v.clone()))
            }
            None => {
                shard.misses.fetch_add(1, Ordering::Relaxed);
                Ok(None)
            }
        }
    }

    fn cache_set(&self, k: K, v: V) -> Result<Option<V>, Self::Error> {
        let shard = self.shard_of(&k);
        Ok(shard.lock.write().insert(k, v))
    }

    fn cache_remove(&self, k: &K) -> Result<Option<V>, Self::Error> {
        ConcurrentCached::cache_remove_entry(self, k).map(|r| r.map(|(_, v)| v))
    }

    fn cache_remove_entry(&self, k: &K) -> Result<Option<(K, V)>, Self::Error> {
        let shard = self.shard_of(k);
        let removed = shard.lock.write().remove_entry(k);
        if let Some((ref stored_k, ref v)) = removed
            && let Some(on_evict) = &self.inner.on_evict
        {
            on_evict(stored_k, v);
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
        }
        Ok(())
    }
}

#[cfg(feature = "async_core")]
impl<K, V, H> ConcurrentCachedAsync<K, V> for ShardedUnboundCacheBase<K, V, H>
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
}

/// Builder for [`ShardedUnboundCacheBase`].
pub struct ShardedUnboundCacheBuilder<K, V, H = DefaultShardHasher> {
    shards: Option<usize>,
    per_shard_initial_capacity: Option<usize>,
    hasher: Option<H>,
    on_evict: Option<OnEvict<K, V>>,
    _k: std::marker::PhantomData<K>,
    _v: std::marker::PhantomData<V>,
}

impl<K, V> Default for ShardedUnboundCacheBuilder<K, V, DefaultShardHasher> {
    fn default() -> Self {
        Self {
            shards: None,
            per_shard_initial_capacity: None,
            hasher: Some(DefaultShardHasher::default()),
            on_evict: None,
            _k: std::marker::PhantomData,
            _v: std::marker::PhantomData,
        }
    }
}

impl<K, V, H> ShardedUnboundCacheBuilder<K, V, H> {
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
    pub fn hasher<H2: ShardHasher<K>>(self, hasher: H2) -> ShardedUnboundCacheBuilder<K, V, H2> {
        ShardedUnboundCacheBuilder {
            shards: self.shards,
            per_shard_initial_capacity: self.per_shard_initial_capacity,
            hasher: Some(hasher),
            on_evict: self.on_evict,
            _k: std::marker::PhantomData,
            _v: std::marker::PhantomData,
        }
    }

    /// Set a callback invoked when an entry is explicitly removed via
    /// [`cache_remove`](ConcurrentCached::cache_remove) or
    /// [`cache_remove_entry`](ConcurrentCached::cache_remove_entry).
    /// Does **not** fire on [`clear`](ShardedUnboundCacheBase::clear);
    /// use [`cache_clear_with_on_evict`](ShardedUnboundCacheBase::cache_clear_with_on_evict) to opt in.
    ///
    /// **Note**: `ShardedUnboundCache` does not track eviction counts — `metrics().evictions` always
    /// returns `None` even when `on_evict` is configured. Use the callback itself to count
    /// evictions if needed.
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
    /// Use [`ShardedUnboundCache::builder()`] (or [`ShardedUnboundCacheBase::builder()`]) to obtain a builder,
    /// configure it, then call `.build()`.
    ///
    /// This builder never fails for valid inputs. The only error case is an
    /// invalid shard count (e.g. `usize::MAX` overflows the next-power-of-two
    /// rounding).
    ///
    /// # Errors
    ///
    /// Returns [`BuildError::InvalidValue`] if the `shards` count overflows
    /// when rounded up to the next power of two.
    #[must_use = "the Result from build() must be used"]
    pub fn build(self) -> Result<ShardedUnboundCacheBase<K, V, H>, BuildError>
    where
        K: Hash + Eq,
        H: ShardHasher<K>,
    {
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
        Ok(ShardedUnboundCacheBase {
            inner: Arc::new(UnboundInner {
                shards,
                shard_mask: mask,
                hasher: self
                    .hasher
                    .expect("hasher is always initialized via Default or .hasher()"),
                on_evict: self.on_evict,
            }),
        })
    }

    /// Build the new cache and copy every entry from `existing` into it.
    ///
    /// Entries are re-hashed through `H` so they land in the correct shards
    /// of the new cache. Acquires each shard's read lock on `existing` one at
    /// a time — `existing` keeps serving concurrent ops throughout.
    ///
    /// Swapping which cache is "live" after the copy is the caller's
    /// responsibility. Requests racing the swap may observe a cache miss.
    ///
    /// **Note**: writes to `existing` that occur after a shard's read lock is
    /// released may or may not appear in the new cache; the new cache warms up
    /// from misses after the swap.
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
        existing: &ShardedUnboundCacheBase<K, V, H2>,
    ) -> Result<ShardedUnboundCacheBase<K, V, H>, BuildError>
    where
        K: Clone + Hash + Eq,
        V: Clone,
        H: ShardHasher<K>,
    {
        let new_cache = self.build()?;
        for shard in existing.inner.shards.iter() {
            let entries: Vec<(K, V)> = {
                let guard = shard.lock.read();
                guard.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
            };
            for (k, v) in entries {
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
    fn new_returns_ready_cache() {
        let c = ShardedUnboundCache::<u32, u32>::new();
        assert_eq!(SyncConcurrentCached::cache_set(&c, 1, 100).unwrap(), None);
        assert_eq!(SyncConcurrentCached::cache_get(&c, &1).unwrap(), Some(100));
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn basic_get_set_remove() {
        let c = ShardedUnboundCache::<u32, u32>::builder().build().unwrap();
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
            SyncConcurrentCached::cache_set(&c, 1, 200).expect("insert must succeed"),
            Some(100)
        );
        assert_eq!(
            SyncConcurrentCached::cache_get(&c, &1).expect("key was just inserted"),
            Some(200)
        );
        assert_eq!(
            SyncConcurrentCached::cache_remove(&c, &1).expect("key must be present"),
            Some(200)
        );
        assert_eq!(
            SyncConcurrentCached::cache_get(&c, &1).expect("cache_get must succeed"),
            None
        );
    }

    #[test]
    fn clone_shares_state() {
        let c1 = ShardedUnboundCache::<u32, u32>::builder().build().unwrap();
        let c2 = c1.clone();
        SyncConcurrentCached::cache_set(&c1, 1, 10).expect("insert must succeed");
        assert_eq!(
            SyncConcurrentCached::cache_get(&c2, &1).expect("key was just inserted"),
            Some(10)
        );
    }

    #[test]
    fn metrics_sum() {
        let c = ShardedUnboundCache::<u32, u32>::builder().build().unwrap();
        SyncConcurrentCached::cache_set(&c, 1, 1).expect("insert must succeed");
        SyncConcurrentCached::cache_get(&c, &1).expect("key was just inserted");
        SyncConcurrentCached::cache_get(&c, &2).expect("cache_get must succeed");
        let m = c.metrics();
        assert_eq!(m.hits, Some(1));
        assert_eq!(m.misses, Some(1));
    }

    #[test]
    fn len_and_clear() {
        let c = ShardedUnboundCache::<u32, u32>::builder().build().unwrap();
        for i in 0..10u32 {
            SyncConcurrentCached::cache_set(&c, i, i).expect("insert must succeed");
        }
        assert_eq!(c.len(), 10);
        assert!(!c.is_empty());
        c.clear();
        assert_eq!(c.len(), 0);
        assert!(c.is_empty());
    }

    #[test]
    fn shard_sizes() {
        let c = ShardedUnboundCache::<u32, u32>::builder()
            .shards(8)
            .build()
            .unwrap();
        for i in 0..100u32 {
            SyncConcurrentCached::cache_set(&c, i, i).expect("insert must succeed");
        }
        let sizes = c.shard_sizes();
        assert_eq!(sizes.len(), 8);
        assert_eq!(sizes.iter().sum::<usize>(), 100);
    }

    #[test]
    fn on_evict_fires_on_remove() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let count = Arc::new(AtomicUsize::new(0));
        let count2 = count.clone();
        let c = ShardedUnboundCacheBase::<u32, u32>::builder()
            .on_evict(move |_, _| {
                count2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1, 1).expect("insert must succeed");
        SyncConcurrentCached::cache_remove(&c, &1).expect("key must be present");
        assert_eq!(count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn custom_hasher() {
        #[derive(Clone, Default)]
        struct ConstHasher;
        impl ShardHasher<u32> for ConstHasher {
            fn shard_hash(&self, _key: &u32) -> u64 {
                0
            }
        }
        let c = ShardedUnboundCacheBase::<u32, u32>::builder()
            .shards(8)
            .hasher(ConstHasher)
            .build()
            .unwrap();
        for i in 0..10u32 {
            SyncConcurrentCached::cache_set(&c, i, i).expect("insert must succeed");
        }
        // All keys route to shard 0
        let sizes = c.shard_sizes();
        assert_eq!(sizes[0], 10);
        assert_eq!(sizes[1..].iter().sum::<usize>(), 0);
    }

    #[test]
    fn copy_from_preserves_entries() {
        let old = ShardedUnboundCache::<u32, u32>::builder().build().unwrap();
        for i in 0..50u32 {
            SyncConcurrentCached::cache_set(&old, i, i * 10).expect("insert must succeed");
        }
        let new_cache = ShardedUnboundCacheBase::<u32, u32>::builder()
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
    fn deep_clone_is_independent() {
        let c1 = ShardedUnboundCache::<u32, u32>::builder().build().unwrap();
        SyncConcurrentCached::cache_set(&c1, 1, 1).expect("insert must succeed");
        let c2 = c1.deep_clone();
        SyncConcurrentCached::cache_set(&c1, 2, 2).expect("insert must succeed");
        assert_eq!(
            SyncConcurrentCached::cache_get(&c2, &2).expect("cache_get must succeed"),
            None
        );
        assert_eq!(
            SyncConcurrentCached::cache_get(&c1, &1).expect("key was just inserted"),
            Some(1)
        );
        assert_eq!(
            SyncConcurrentCached::cache_get(&c2, &1).expect("key was copied to deep clone"),
            Some(1)
        );
    }

    #[test]
    fn send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ShardedUnboundCache<u32, u32>>();
    }

    #[test]
    fn build_error_on_overflow() {
        let c = ShardedUnboundCacheBase::<u32, u32>::builder()
            .shards(usize::MAX)
            .build();
        assert!(c.is_err());
        match c.expect_err("usize::MAX shards should fail") {
            BuildError::InvalidValue { field, reason } => {
                assert_eq!(field, "shards");
                assert!(reason.contains("overflows"));
            }
            _ => panic!("expected BuildError::InvalidValue"),
        }
    }

    #[test]
    fn build_error_on_zero_shards() {
        let c = ShardedUnboundCacheBase::<u32, u32>::builder()
            .shards(0)
            .build();
        assert!(c.is_err(), "zero shards should return Err");
        match c.expect_err("zero shards should fail") {
            BuildError::InvalidValue { field, .. } => {
                assert_eq!(field, "shards");
            }
            _ => panic!("expected BuildError::InvalidValue"),
        }
    }

    #[test]
    fn cache_clear_with_on_evict_fires_for_all_entries() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let count = Arc::new(AtomicUsize::new(0));
        let count2 = count.clone();
        let c = ShardedUnboundCacheBase::<u32, u32>::builder()
            .on_evict(move |_, _| {
                count2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();
        for i in 0..20u32 {
            SyncConcurrentCached::cache_set(&c, i, i).expect("insert must succeed");
        }
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
    }

    #[test]
    fn clear_does_not_fire_on_evict() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let count = Arc::new(AtomicUsize::new(0));
        let count2 = count.clone();
        let c = ShardedUnboundCacheBase::<u32, u32>::builder()
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
        let c = ShardedUnboundCacheBase::<u32, u32>::builder()
            .shards(1)
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
        use std::sync::atomic::{AtomicUsize, Ordering};
        let count = Arc::new(AtomicUsize::new(0));
        let count2 = count.clone();
        let c = ShardedUnboundCacheBase::<u32, u32>::builder()
            .shards(1)
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
    fn cache_delete_returns_true_for_present_entry() {
        let c = ShardedUnboundCacheBase::<u32, u32>::builder()
            .shards(1)
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1u32, 10u32).expect("insert must succeed");
        assert!(SyncConcurrentCached::cache_delete(&c, &1u32).expect("cache_delete must succeed"));
        assert!(!SyncConcurrentCached::cache_delete(&c, &1u32).expect("cache_delete must succeed"));
    }

    // --- Inherent infallible method tests ---

    #[test]
    fn inherent_get_returns_option_not_result() {
        let c = ShardedUnboundCache::<u32, u32>::new();
        // Return type is Option<V> -- no .unwrap() or ? needed.
        let v: Option<u32> = c.get(&1);
        assert_eq!(v, None);
        c.set(1, 42);
        let v: Option<u32> = c.get(&1);
        assert_eq!(v, Some(42));
    }

    #[test]
    fn inherent_set_returns_previous_value() {
        let c = ShardedUnboundCache::<u32, u32>::new();
        // First insert returns None (no prior value).
        let prev: Option<u32> = c.set(1, 10);
        assert_eq!(prev, None);
        // Overwrite returns the old value.
        let prev: Option<u32> = c.set(1, 20);
        assert_eq!(prev, Some(10));
        assert_eq!(c.get(&1), Some(20));
    }

    #[test]
    fn inherent_remove_returns_prior_value() {
        let c = ShardedUnboundCache::<u32, u32>::new();
        c.set(1, 99);
        let v: Option<u32> = c.remove(&1);
        assert_eq!(v, Some(99));
        // Absent key returns None.
        assert_eq!(c.remove(&1), None);
        assert_eq!(c.get(&1), None);
    }

    #[test]
    fn inherent_remove_entry_returns_key_and_value() {
        let c = ShardedUnboundCache::<u32, u32>::new();
        c.set(7, 77);
        let pair: Option<(u32, u32)> = c.remove_entry(&7);
        assert_eq!(pair, Some((7, 77)));
        // Absent key returns None.
        assert_eq!(c.remove_entry(&7), None);
    }

    #[test]
    fn inherent_delete_returns_bool() {
        let c = ShardedUnboundCache::<u32, u32>::new();
        c.set(1, 10);
        let removed: bool = c.delete(&1);
        assert!(removed);
        let removed: bool = c.delete(&1);
        assert!(!removed);
    }

    #[test]
    fn inherent_reset_clears_and_resets_metrics() {
        let c = ShardedUnboundCache::<u32, u32>::new();
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
        // Verify that generic code over ConcurrentCached still works via the trait path
        // even though the inherent `get`/`set`/`remove` methods shadow the trait aliases.
        fn use_trait<C>(cache: &C, k: u32, v: u32)
        where
            C: SyncConcurrentCached<u32, u32>,
        {
            let _: Result<Option<u32>, _> = ConcurrentCached::cache_set(cache, k, v);
            let _: Result<Option<u32>, _> = ConcurrentCached::cache_get(cache, &k);
            let _: Result<Option<u32>, _> = ConcurrentCached::cache_remove(cache, &k);
        }
        let c = ShardedUnboundCache::<u32, u32>::new();
        use_trait(&c, 1, 100);
    }
}
