use std::hash::Hash;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(feature = "ahash")]
use ahash::RandomState;
#[cfg(not(feature = "ahash"))]
use std::collections::hash_map::RandomState;

use std::collections::HashMap;

use crate::{CacheMetrics, ConcurrentCacheBase, ConcurrentCachePeek, ConcurrentCached};
#[cfg(feature = "async_core")]
use crate::{ConcurrentCachePeekAsync, ConcurrentCachedAsync};
#[cfg(feature = "async_core")]
use core::future::Future;

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
/// Use [`deep_clone`](ShardedUnboundCache::deep_clone) to get an independent copy.
///
/// **Note**: reads return owned values cloned from under the shard lock, so `V` must
/// implement `Clone`.
///
/// The shard-selection hasher `H` defaults to [`DefaultShardHasher`] (ahash-backed when the
/// `ahash` feature is enabled, otherwise `std::collections::hash_map::RandomState`), so
/// `ShardedUnboundCache<K, V>` names the common case. To use a custom [`ShardHasher`], call
/// [`ShardedUnboundCache::builder()`] and then
/// [`hasher`](ShardedUnboundCacheBuilder::hasher), which switches `H` to your hasher.
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
pub struct ShardedUnboundCache<K, V, H = DefaultShardHasher> {
    inner: Arc<UnboundInner<K, V, H>>,
}

impl<K, V, H> Clone for ShardedUnboundCache<K, V, H> {
    /// Arc-share clone — both handles point to the same underlying cache.
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<K, V, H> std::fmt::Debug for ShardedUnboundCache<K, V, H> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShardedUnboundCache")
            .field("shards", &self.inner.shards.len())
            .finish_non_exhaustive()
    }
}

impl<K, V> ShardedUnboundCache<K, V, DefaultShardHasher>
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
    /// builder's hasher type and `build` then yields a `ShardedUnboundCache<K, V, H>` over that
    /// hasher. `new` and `builder` exist only on the default-hasher instantiation
    /// `ShardedUnboundCache<K, V, DefaultShardHasher>`, so a custom hasher is always introduced
    /// via `hasher`, never a `ShardedUnboundCache::<_, _, H>` turbofish.
    #[must_use]
    pub fn builder() -> ShardedUnboundCacheBuilder<K, V, DefaultShardHasher> {
        ShardedUnboundCacheBuilder::default()
    }
}

impl<K, V, H> ShardedUnboundCache<K, V, H>
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

impl<K: Clone + Hash + Eq, V: Clone, H: ShardHasher<K>> ShardedUnboundCache<K, V, H> {
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
                    evictions: AtomicU64::new(0),
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

impl<K, V, H: ShardHasher<K>> ShardedUnboundCache<K, V, H>
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
    /// no hit/miss metrics. The single-owner counterpart is
    /// [`CachedPeek::cache_peek`](crate::CachedPeek::cache_peek); the sharded stores
    /// return a clone rather than a reference because the value lives behind a
    /// per-shard lock.
    #[must_use]
    pub fn peek(&self, k: &K) -> Option<V> {
        self.shard_of(k).lock.read().get(k).cloned()
    }
}

impl<K, V, H: ShardHasher<K>> ShardedUnboundCache<K, V, H>
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

    /// Removes entries for which `keep` returns `false`.
    ///
    /// `ShardedUnboundCache` has no expiry dimension and tracks no eviction counter
    /// (`metrics().evictions` is always `None`), so `retain` is a plain predicate filter
    /// over the stored entries: an entry survives exactly when `keep` returns `true`.
    /// Each removed entry fires the configured `on_evict` callback and bumps no counter.
    /// The single-owner counterpart is [`UnboundCache::retain`](crate::UnboundCache::retain).
    /// The expiry-aware sharded stores — [`ShardedTtlCache`](crate::ShardedTtlCache) and
    /// [`ShardedExpiringCache`](crate::ShardedExpiringCache) — have `retain` too, with one
    /// difference: their expired entries are removed regardless of the predicate.
    ///
    /// Returns the total number of entries removed across all shards for this call -- on this
    /// store that is exactly the number of `keep` rejections, since there is no expiry dimension
    /// to fold in. Not `#[must_use]`: discarding the count is a legitimate and common use.
    ///
    /// **Not atomic across shards**: shards are locked and swept **one at a time**, never all
    /// at once (matching [`clear`](Self::clear) and
    /// [`cache_clear_with_on_evict`](Self::cache_clear_with_on_evict)). A concurrent writer can
    /// insert into a shard this call has already visited, and that entry is not filtered.
    ///
    /// `keep` runs while the shard's write lock is held, so it must not re-enter this cache —
    /// the same rule the builder states for `on_evict` — or it will deadlock. `on_evict` fires
    /// **after** the shard lock is released, once per removed entry, in shard order. Because
    /// callbacks run between shard sweeps, an `on_evict` that inserts into a shard this call has
    /// not yet visited will have that entry filtered by the same in-flight `retain`.
    ///
    /// # Panicking predicate
    ///
    /// If `keep` panics, nothing has been removed yet from the shard it panicked in (or from
    /// any shard not yet visited): the sweep of a shard runs `keep` in a first pass that only
    /// *selects* doomed entries and removes them in a second pass that runs no user code.
    /// Shards already swept keep their removals, all of which were counted and notified before
    /// the panic.
    pub fn retain<F: FnMut(&K, &V) -> bool>(&self, mut keep: F) -> usize {
        let mut total_removed = 0usize;
        for shard in self.inner.shards.iter() {
            // Collect under the write lock, fire callbacks after releasing it. Two phases: the
            // first runs `keep` (user code) and only selects, the second removes and runs
            // nothing that can panic. See `stores::take_doomed`.
            let removed: Vec<(K, V)> = {
                let mut guard = shard.lock.write();
                crate::stores::take_doomed(&mut guard, |k, v| !keep(k, v))
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
}

impl<K, V, H> ConcurrentCacheBase for ShardedUnboundCache<K, V, H>
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

impl<K, V, H> ConcurrentCached<K, V> for ShardedUnboundCache<K, V, H>
where
    K: Hash + Eq,
    V: Clone,
    H: ShardHasher<K>,
{
    fn cache_get(&self, k: &K) -> Result<Option<V>, Self::Error> {
        let shard = self.shard_of(k);
        let guard = shard.lock.read();
        let found = guard.get(k).cloned();
        drop(guard);
        match found {
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
        let mut guard = shard.lock.write();
        // `HashMap::insert` keeps the stored key and drops the caller's.
        Ok(guard.insert(k, v))
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

    /// Efficient peek-based contains: acquires a read lock, does not clone the value,
    /// and does not record hit/miss metrics.
    fn cache_contains(&self, k: &K) -> Result<bool, Self::Error> {
        let shard = self.shard_of(k);
        Ok(shard.lock.read().contains_key(k))
    }
}

impl<K, V, H> ConcurrentCachePeek<K, V> for ShardedUnboundCache<K, V, H>
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
impl<K, V, H> ConcurrentCachePeekAsync<K, V> for ShardedUnboundCache<K, V, H>
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
impl<K, V, H> ConcurrentCachedAsync<K, V> for ShardedUnboundCache<K, V, H>
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

    /// Efficient peek-based contains: does not clone the value and does not record
    /// hit/miss metrics.
    fn async_cache_contains(&self, k: &K) -> impl Future<Output = Result<bool, Self::Error>> + Send
    where
        Self: Sized + Sync,
        K: Sync,
    {
        let result = ConcurrentCached::cache_contains(self, k);
        async move { result }
    }
}

/// Builder for [`ShardedUnboundCache`].
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

impl<K, V> ShardedUnboundCacheBuilder<K, V> {
    /// Create a builder with default settings. Equivalent to [`ShardedUnboundCache::builder`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
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
    /// Does **not** fire on [`clear`](ShardedUnboundCache::clear);
    /// use [`cache_clear_with_on_evict`](ShardedUnboundCache::cache_clear_with_on_evict) to opt in.
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
    /// Use [`ShardedUnboundCache::builder()`] to obtain a builder, configure it, then call
    /// `.build()`.
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
    pub fn build(self) -> Result<ShardedUnboundCache<K, V, H>, BuildError>
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
        Ok(ShardedUnboundCache {
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
        existing: &ShardedUnboundCache<K, V, H2>,
    ) -> Result<ShardedUnboundCache<K, V, H>, BuildError>
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
    fn default_shard_count_is_plain_default_no_capacity_scaling() {
        use crate::stores::sharded::default_shard_count;
        // The unbounded store has no max_size to scale against, so the capacity-derived cap
        // never applies: the default path keeps the plain default_shard_count().
        let c = ShardedUnboundCache::<u32, u32>::builder().build().unwrap();
        assert_eq!(c.shards(), default_shard_count());
        // An explicit .shards(n) still rounds up to a power of two and is authoritative.
        let c2 = ShardedUnboundCache::<u32, u32>::builder()
            .shards(10)
            .build()
            .unwrap();
        assert_eq!(c2.shards(), 16);
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
        let c = ShardedUnboundCache::<u32, u32>::builder()
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
        let c = ShardedUnboundCache::<u32, u32>::builder()
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
        let new_cache = ShardedUnboundCache::<u32, u32>::builder()
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
        let c = ShardedUnboundCache::<u32, u32>::builder()
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
        let c = ShardedUnboundCache::<u32, u32>::builder().shards(0).build();
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
        let c = ShardedUnboundCache::<u32, u32>::builder()
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
        let c = ShardedUnboundCache::<u32, u32>::builder()
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
        // removed pairs under the shard write guard, drops it, and only then fires
        // `on_evict`. `try_write` returning `None` for any shard would mean the guard
        // was still held while the callback ran.
        use std::sync::OnceLock;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let handle: Arc<OnceLock<ShardedUnboundCache<u32, u32>>> = Arc::new(OnceLock::new());
        let handle2 = handle.clone();
        let fired = Arc::new(AtomicUsize::new(0));
        let fired2 = fired.clone();
        let c = ShardedUnboundCache::<u32, u32>::builder()
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
            c.set(i, i);
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
    fn retain_sweeps_every_shard_one_at_a_time() {
        // Per-shard bookkeeping: the predicate must be applied to the entries of each
        // shard's own map, so the post-retain per-shard counts equal the number of
        // surviving keys routed to that shard.
        let c = ShardedUnboundCache::<u32, u32>::builder()
            .shards(4)
            .build()
            .unwrap();
        for i in 0..64u32 {
            c.set(i, i);
        }
        let expected: Vec<usize> = c
            .inner
            .shards
            .iter()
            .map(|s| s.lock.read().keys().filter(|k| *k % 2 == 0).count())
            .collect();
        c.retain(|k, _v| k % 2 == 0);
        assert_eq!(c.shard_sizes(), expected);
        assert_eq!(expected.iter().sum::<usize>(), 32);
    }

    #[test]
    fn cache_remove_entry_basic() {
        let c = ShardedUnboundCache::<u32, u32>::builder()
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
        let c = ShardedUnboundCache::<u32, u32>::builder()
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
        let c = ShardedUnboundCache::<u32, u32>::builder()
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
