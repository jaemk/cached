use std::hash::Hash;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(feature = "async_core")]
use crate::ConcurrentCachedAsync;
use crate::{
    CacheMetrics, CachedIter, CachedPeek, ConcurrentCached, ConcurrentCloneCached, Expires,
};

use super::{
    CachePadded, DefaultShardHasher, Shard, ShardHasher, checked_shard_count, shard_index,
};
use crate::Cached;
use crate::ConcurrentCacheEvict;
use crate::stores::{BuildError, LruCache};

type OnEvict<K, V> = Arc<dyn Fn(&K, &V) + Send + Sync>;

#[allow(clippy::type_complexity)]
struct ExpiringLruInner<K, V, H> {
    shards: Box<[CachePadded<Shard<LruCache<K, V>>>]>,
    shard_mask: usize,
    hasher: H,
    on_evict: Option<OnEvict<K, V>>,
    evictions: AtomicU64,
    total_capacity: usize,
}

/// A fully-concurrent, partitioned, LRU size-bounded in-memory cache with per-value expiry.
///
/// Each value controls its own expiration by implementing [`Expires`]. Expired entries
/// are checked on lookup and evicted on access or during explicit [`evict`](ConcurrentCacheEvict::evict) sweeps.
/// Eviction is also enforced independently per shard when capacity limits are hit.
///
/// Wraps an `Arc` — `clone()` is an Arc-share (shared state), not a deep copy.
/// Use [`deep_clone`](ShardedExpiringLruCacheBase::deep_clone) to get an independent copy.
///
/// **Note**: `K` and `V` must implement `Clone` (`K` for LRU key tracking; `V` because reads
/// return owned values cloned from under the shard lock, in addition to `V: Expires`).
///
/// This is a type alias for `ShardedExpiringLruCacheBase<K, V, DefaultShardHasher>`.
/// To use a custom shard hasher, call [`ShardedExpiringLruCache::builder()`] and then
/// [`hasher`](ShardedExpiringLruCacheBuilder::hasher), which yields a
/// `ShardedExpiringLruCacheBase<K, V, H>` over your hasher.
///
/// **Note**: Setting an `on_evict` callback requires the callback itself to be `'static` because
/// the cache stores it behind an `Arc<dyn Fn(&K, &V) + Send + Sync>`. This does not add `'static`
/// bounds to `K` or `V`.
pub type ShardedExpiringLruCache<K, V> = ShardedExpiringLruCacheBase<K, V, DefaultShardHasher>;

/// Backing type for [`ShardedExpiringLruCache`] with a generic shard hasher `H`.
pub struct ShardedExpiringLruCacheBase<K, V, H = DefaultShardHasher> {
    inner: Arc<ExpiringLruInner<K, V, H>>,
}

impl<K, V, H> Clone for ShardedExpiringLruCacheBase<K, V, H> {
    /// Arc-share clone — both handles point to the same underlying cache.
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<K, V, H> std::fmt::Debug for ShardedExpiringLruCacheBase<K, V, H> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShardedExpiringLruCache")
            .field("shards", &self.inner.shards.len())
            .field("capacity", &self.inner.total_capacity)
            .field("evictions", &self.inner.evictions.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl<K, V> ShardedExpiringLruCacheBase<K, V, DefaultShardHasher>
where
    K: Hash + Eq + Clone,
    V: Expires,
{
    /// Construct a ready-to-use [`ShardedExpiringLruCache`] holding up to roughly `max_size`
    /// entries total, with the [`DefaultShardHasher`] and a default shard count.
    ///
    /// Note that the effective total capacity can exceed `max_size` for small values
    /// because each shard reserves a minimum capacity (see
    /// [`max_size`](ShardedExpiringLruCacheBuilder::max_size)). For a custom hasher, shard
    /// count, per-shard cap, or `on_evict`, use [`builder`](Self::builder).
    ///
    /// # Panics
    ///
    /// Panics if `max_size` is `0`, or if the effective sharded capacity overflows
    /// `usize` / a per-shard allocation fails. Use [`builder`](Self::builder) with
    /// [`build`](ShardedExpiringLruCacheBuilder::build) to handle those cases without panicking.
    #[must_use]
    pub fn new(max_size: usize) -> ShardedExpiringLruCache<K, V> {
        Self::builder().max_size(max_size).build().expect(
            "ShardedExpiringLruCache::new requires a non-zero max_size with a valid allocation",
        )
    }

    /// Return a builder for constructing a [`ShardedExpiringLruCache`].
    ///
    /// The builder starts with the [`DefaultShardHasher`]. To use a custom hasher, call
    /// [`hasher`](ShardedExpiringLruCacheBuilder::hasher) on the returned builder; it switches
    /// the builder's hasher type and `build` then yields a `ShardedExpiringLruCacheBase` over
    /// that hasher. `new` and `builder` exist only on the default-hasher alias, so a custom
    /// hasher is always introduced via `hasher`, never a
    /// `ShardedExpiringLruCacheBase::<_, _, H>` turbofish.
    #[must_use]
    pub fn builder() -> ShardedExpiringLruCacheBuilder<K, V, DefaultShardHasher> {
        ShardedExpiringLruCacheBuilder::default()
    }
}

impl<K, V, H> ShardedExpiringLruCacheBase<K, V, H>
where
    K: Hash + Eq + Clone,
    V: Expires,
    H: ShardHasher<K>,
{
    #[inline]
    fn shard_of(&self, k: &K) -> &CachePadded<Shard<LruCache<K, V>>> {
        let h = self.inner.hasher.shard_hash(k);
        &self.inner.shards[shard_index(h, self.inner.shard_mask)]
    }
}

impl<K: Clone + Hash + Eq, V: Clone + Expires, H: ShardHasher<K> + Clone>
    ShardedExpiringLruCacheBase<K, V, H>
{
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
                drop(guard);
                let hits = self.inner.shards[i].hits.load(Ordering::Relaxed);
                let misses = self.inner.shards[i].misses.load(Ordering::Relaxed);
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
            inner: Arc::new(ExpiringLruInner {
                shards,
                shard_mask: self.inner.shard_mask,
                hasher: self.inner.hasher.clone(),
                on_evict: self.inner.on_evict.clone(),
                evictions: AtomicU64::new(self.inner.evictions.load(Ordering::Relaxed)),
                total_capacity: self.inner.total_capacity,
            }),
        }
    }
}

impl<K, V, H: ShardHasher<K>> ShardedExpiringLruCacheBase<K, V, H>
where
    K: Hash + Eq + Clone,
    V: Expires,
{
    /// Return aggregate metrics across all shards.
    ///
    /// `evictions` aggregates every entry removal that fires (or would fire) `on_evict`,
    /// across all shards:
    /// - LRU capacity evictions during [`cache_set`](ConcurrentCached::cache_set);
    /// - explicit removes via [`cache_remove`](ConcurrentCached::cache_remove) and
    ///   [`cache_remove_entry`](ConcurrentCached::cache_remove_entry);
    /// - bulk removal via [`cache_clear_with_on_evict`](Self::cache_clear_with_on_evict)
    ///   (but **not** [`clear`](Self::clear), which is silent);
    /// - expired entries dropped lazily on access during
    ///   [`cache_get`](ConcurrentCached::cache_get);
    /// - expired entries swept by [`evict`](Self::evict).
    ///
    /// `capacity` reflects the effective total capacity — may exceed the requested
    /// `size` when the 16-per-shard minimum floor is applied; see [`capacity`](Self::capacity).
    #[must_use]
    pub fn metrics(&self) -> CacheMetrics {
        let mut hits = 0u64;
        let mut misses = 0u64;
        let mut inner_evictions = 0u64;
        let mut size = 0usize;
        for shard in self.inner.shards.iter() {
            hits += shard.hits.load(Ordering::Relaxed);
            misses += shard.misses.load(Ordering::Relaxed);
            let guard = shard.lock.read();
            if let Some(e) = guard.cache_evictions() {
                inner_evictions += e;
            }
            size += guard.cache_size();
        }
        CacheMetrics {
            hits: Some(hits),
            misses: Some(misses),
            evictions: Some(inner_evictions + self.inner.evictions.load(Ordering::Relaxed)),
            entry_count: size,
            capacity: Some(self.inner.total_capacity),
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
            .map(|s| s.lock.read().cache_size())
            .collect()
    }

    /// Total number of entries across all shards (including not-yet-swept expired entries).
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
    /// When constructed with [`max_size`](ShardedExpiringLruCacheBuilder::max_size), this may
    /// be larger than the requested size because per-shard capacity is rounded
    /// up with ceiling division.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.inner.total_capacity
    }
}

impl<K, V, H> ConcurrentCached<K, V> for ShardedExpiringLruCacheBase<K, V, H>
where
    K: Hash + Eq + Clone,
    V: Clone + Expires,
    H: ShardHasher<K>,
{
    type Error = std::convert::Infallible;

    fn cache_get(&self, k: &K) -> Result<Option<V>, Self::Error> {
        let shard = self.shard_of(k);
        let mut guard = shard.lock.write();
        let expired = match guard.cache_peek(k) {
            None => {
                shard.misses.fetch_add(1, Ordering::Relaxed);
                return Ok(None);
            }
            Some(v) => v.is_expired(),
        };

        if expired {
            let removed = guard.pop_raw(k);
            drop(guard);
            if let Some((ref key, ref val)) = removed {
                // `pop_raw` removes the entry without bumping the inner LRU eviction counter,
                // so track expired-on-access removals in the outer counter instead. Explicit
                // removes via `cache_remove` bump the inner LRU counter (`guard.evictions`).
                // Both paths feed into `metrics().evictions` via the combined sum in `metrics()`.
                self.inner.evictions.fetch_add(1, Ordering::Relaxed);
                if let Some(on_evict) = &self.inner.on_evict {
                    on_evict(key, val);
                }
            }
            shard.misses.fetch_add(1, Ordering::Relaxed);
            Ok(None)
        } else {
            // Live hit — update LRU recency and extract value
            let val = guard.cache_get(k).cloned();
            shard.hits.fetch_add(1, Ordering::Relaxed);
            Ok(val)
        }
    }

    fn cache_set(&self, k: K, v: V) -> Result<Option<V>, Self::Error> {
        let shard = self.shard_of(&k);
        Ok(shard.lock.write().cache_set(k, v))
    }

    fn cache_remove(&self, k: &K) -> Result<Option<V>, Self::Error> {
        let shard = self.shard_of(k);
        let removed = {
            let mut guard = shard.lock.write();
            let removed = guard.pop_raw(k);
            if removed.is_some() {
                guard.evictions.fetch_add(1, Ordering::Relaxed);
            }
            removed
        };
        let Some((key, val)) = removed else {
            return Ok(None);
        };
        if let Some(on_evict) = &self.inner.on_evict {
            on_evict(&key, &val);
        }
        if val.is_expired() {
            Ok(None)
        } else {
            Ok(Some(val))
        }
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
        let Some((key, val)) = removed else {
            return Ok(None);
        };
        if let Some(on_evict) = &self.inner.on_evict {
            on_evict(&key, &val);
        }
        Ok(Some((key, val)))
    }

    fn cache_size(&self) -> Result<Option<usize>, Self::Error> {
        Ok(Some(self.len()))
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
            // Zero the per-shard inner store's metrics, including its LRU capacity-eviction counter.
            shard.lock.write().cache_reset_metrics();
        }
        self.inner.evictions.store(0, Ordering::Relaxed);
        Ok(())
    }

    /// No-op: this store uses value-defined expiry, not a refreshable TTL. Always returns `false`.
    fn set_refresh_on_hit(&self, _refresh: bool) -> bool {
        false
    }
}

#[cfg(feature = "async_core")]
impl<K, V, H> ConcurrentCachedAsync<K, V> for ShardedExpiringLruCacheBase<K, V, H>
where
    K: Hash + Eq + Clone + Send + Sync,
    V: Clone + Expires + Send + Sync,
    H: ShardHasher<K>,
{
    type Error = std::convert::Infallible;

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

    fn cache_size(&self) -> Result<Option<usize>, Self::Error> {
        Ok(Some(self.len()))
    }

    fn set_refresh_on_hit(&self, b: bool) -> bool {
        <Self as ConcurrentCached<K, V>>::set_refresh_on_hit(self, b)
    }
}

impl<K, V, H> ShardedExpiringLruCacheBase<K, V, H>
where
    K: Clone + Hash + Eq,
    V: Expires,
    H: ShardHasher<K>,
{
    /// Sweep all shards for expired entries, remove them, fire the `on_evict` callback
    /// (if set) for each, and return the total count of removed entries.
    #[must_use]
    pub fn evict(&self) -> usize {
        let mut total = 0;
        for shard in self.inner.shards.iter() {
            let removed = {
                let mut guard = shard.lock.write();
                let expired_keys: Vec<K> = guard
                    .iter()
                    .filter(|(_, v)| v.is_expired())
                    .map(|(k, _)| k.clone())
                    .collect();
                let mut removed = Vec::new();
                for k in expired_keys {
                    if let Some((key, val)) = guard.pop_raw(&k) {
                        removed.push((key, val));
                    }
                }
                removed
            };

            total += removed.len();
            if !removed.is_empty() {
                self.inner
                    .evictions
                    .fetch_add(removed.len() as u64, Ordering::Relaxed);
                if let Some(on_evict) = &self.inner.on_evict {
                    for (k, v) in &removed {
                        on_evict(k, v);
                    }
                }
            }
        }
        total
    }
}

impl<K, V, H> ConcurrentCacheEvict for ShardedExpiringLruCacheBase<K, V, H>
where
    K: Clone + Hash + Eq,
    V: Expires,
    H: ShardHasher<K>,
{
    fn evict(&self) -> usize {
        ShardedExpiringLruCacheBase::evict(self)
    }
}

/// Builder for [`ShardedExpiringLruCacheBase`].
///
/// Note: there is intentionally **no `.ttl()` setter**. A sharded expiring LRU cache has no
/// global expiry duration — each value decides when it is expired via the [`Expires`] trait,
/// while `max_size` bounds the entry count via LRU. For a single global TTL applied to every
/// entry, use [`ShardedLruTtlCache`](crate::ShardedLruTtlCache) instead.
#[doc(alias = "ttl")]
pub struct ShardedExpiringLruCacheBuilder<K, V: Expires, H = DefaultShardHasher> {
    shards: Option<usize>,
    max_size: Option<usize>,
    per_shard_max_size: Option<usize>,
    hasher: Option<H>,
    on_evict: Option<OnEvict<K, V>>,
    _k: std::marker::PhantomData<K>,
    _v: std::marker::PhantomData<V>,
}

impl<K, V: Expires> Default for ShardedExpiringLruCacheBuilder<K, V, DefaultShardHasher> {
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

impl<K, V: Expires, H> ShardedExpiringLruCacheBuilder<K, V, H> {
    /// Set the requested total capacity (divided across shards via `div_ceil`).
    /// Mutually exclusive with [`per_shard_max_size`](Self::per_shard_max_size).
    ///
    /// Eviction is enforced independently per shard. Each shard gets
    /// `ceil(size / shards)` entries, with a minimum of 16 per shard when
    /// `shards > 1` (see the **Capacity Fragmentation Warning** on
    /// [`ShardedExpiringLruCacheBuilder::max_size`]).
    ///
    /// # Minimum capacity
    ///
    /// Because each shard reserves a minimum of **16** entries when `shards > 1`, the effective
    /// total capacity is at least `shards * 16` and may **exceed** the requested `max_size` for
    /// small values (e.g. `max_size = 10` with 8 shards yields an effective capacity of 128).
    /// [`metrics()`](ShardedExpiringLruCacheBase::metrics)'s `capacity` and `entry_count` reflect
    /// the actual (possibly larger) amount. Use [`per_shard_max_size`](Self::per_shard_max_size)
    /// or `shards = 1` if you need a strict small cap.
    ///
    /// Use [`per_shard_max_size`](Self::per_shard_max_size) for an exact per-shard cap instead.
    #[doc(alias = "size")]
    #[doc(alias = "capacity")]
    #[must_use]
    pub fn max_size(mut self, max_size: usize) -> Self {
        self.max_size = Some(max_size);
        self
    }

    /// Set per-shard capacity directly. Mutually exclusive with [`max_size`](Self::max_size).
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
    pub fn hasher<H2: ShardHasher<K>>(
        self,
        hasher: H2,
    ) -> ShardedExpiringLruCacheBuilder<K, V, H2> {
        ShardedExpiringLruCacheBuilder {
            shards: self.shards,
            max_size: self.max_size,
            per_shard_max_size: self.per_shard_max_size,
            hasher: Some(hasher),
            on_evict: self.on_evict,
            _k: std::marker::PhantomData,
            _v: std::marker::PhantomData,
        }
    }

    /// Set a callback invoked when an entry is evicted. Fires for LRU capacity evictions,
    /// expired-entry removal during [`cache_get`](ConcurrentCached::cache_get), explicitly via
    /// [`evict`](ShardedExpiringLruCacheBase::evict), on explicit
    /// [`cache_remove`](ConcurrentCached::cache_remove), and on
    /// [`cache_remove_entry`](ConcurrentCached::cache_remove_entry).
    /// Does **not** fire on [`clear`](ShardedExpiringLruCacheBase::clear);
    /// use [`cache_clear_with_on_evict`](ShardedExpiringLruCacheBase::cache_clear_with_on_evict) to opt in.
    ///
    /// Capacity-eviction callbacks run while the affected shard's write lock is held. Do not call
    /// methods on the same sharded cache from the callback; doing so can deadlock if the callback
    /// re-enters the locked shard. Expiry sweeps via [`evict`](ShardedExpiringLruCacheBase::evict)
    /// and explicit removes via [`cache_remove`](ConcurrentCached::cache_remove) /
    /// [`cache_remove_entry`](ConcurrentCached::cache_remove_entry) fire `on_evict` after
    /// releasing the shard lock and do not have this restriction.
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

    /// Build the new cache and copy every non-expired entry from `existing` into it,
    /// preserving LRU ordering (least-recently-used entries inserted first so that
    /// most-recently-used entries end up at the head of the new cache).
    ///
    /// Acquires each shard's read lock on `existing` one at a time — `existing`
    /// keeps serving concurrent ops throughout. Entries whose
    /// [`is_expired`](crate::Expires::is_expired) returns `true` at copy time are
    /// skipped and not transferred. Entries that cannot fit in the new per-shard
    /// capacity are evicted (LRU-first), firing `on_evict` on the NEW cache's
    /// callback if set.
    ///
    /// **Note**: `on_evict` callbacks on `existing` do not fire — entries are read
    /// (not removed) from the source cache.
    ///
    /// # Panics
    ///
    /// Panics if the builder configuration is invalid (e.g. `max_size` / `per_shard_max_size`
    /// not set or is `0`, or both set simultaneously). Prefer calling
    /// [`build`](Self::build) directly to handle errors without panicking.
    #[must_use]
    pub fn copy_from<H2: ShardHasher<K>>(
        self,
        existing: &ShardedExpiringLruCacheBase<K, V, H2>,
    ) -> ShardedExpiringLruCacheBase<K, V, H>
    where
        K: Clone + Hash + Eq,
        V: Clone,
        H: ShardHasher<K>,
    {
        let new_cache = self
            .build()
            .unwrap_or_else(|e| panic!("ShardedExpiringLruCache build failed: {e}"));
        for shard in existing.inner.shards.iter() {
            // iter_order returns MRU-first; insert in reverse (LRU-first) so
            // that MRU entries land at the head of the new cache.
            let entries: Vec<(K, V)> = {
                let guard = shard.lock.read();
                guard.iter_order()
            };
            for (k, v) in entries.into_iter().rev() {
                if !v.is_expired() {
                    let _ = ConcurrentCached::cache_set(&new_cache, k, v);
                }
            }
        }
        new_cache
    }

    /// Build the cache, returning an error if required fields are missing or invalid.
    ///
    /// Use [`ShardedExpiringLruCache::builder()`] (or [`ShardedExpiringLruCacheBase::builder()`])
    /// to obtain a builder, set at least [`max_size`](Self::max_size), then call `.build()`.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError`] if `size` (or `per_shard_max_size`) was not set, is `0`,
    /// or if both `max_size` and `per_shard_max_size` are set simultaneously,
    /// or if the shard count overflows.
    #[must_use = "the Result from build() must be used"]
    pub fn build(self) -> Result<ShardedExpiringLruCacheBase<K, V, H>, BuildError>
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
        Ok(ShardedExpiringLruCacheBase {
            inner: Arc::new(ExpiringLruInner {
                shards,
                shard_mask: mask,
                hasher: self
                    .hasher
                    .expect("hasher is always initialized via Default or .hasher()"),
                on_evict: self.on_evict,
                evictions: AtomicU64::new(0),
                total_capacity: total_cap,
            }),
        })
    }
}

impl<K, V, H> ConcurrentCloneCached<K, V> for ShardedExpiringLruCacheBase<K, V, H>
where
    K: Hash + Eq + Clone,
    V: Clone + Expires,
    H: ShardHasher<K>,
{
    /// Returns `(Some(v), false)` for a live entry (hit, LRU promoted), `(Some(v), true)` for an
    /// expired entry (miss, **no removal**, no LRU promotion, no eviction counter), or
    /// `(None, false)` when absent (miss).
    fn cache_get_with_expiry_status(&self, k: &K) -> (Option<V>, bool) {
        let shard = self.shard_of(k);
        let mut guard = shard.lock.write();
        // Single peek captures both expiry status and value; the expired path
        // can then return without a second lookup.
        let (expired, peeked) = match guard.cache_peek(k) {
            None => {
                drop(guard);
                shard.misses.fetch_add(1, Ordering::Relaxed);
                return (None, false);
            }
            Some(v) => (v.is_expired(), v.clone()),
        };
        if expired {
            // Return stale value without removing the entry, promoting LRU recency,
            // or touching eviction counters.
            drop(guard);
            shard.misses.fetch_add(1, Ordering::Relaxed);
            (Some(peeked), true)
        } else {
            // Live hit — promote LRU recency via cache_get.
            let value = guard.cache_get(k).cloned();
            drop(guard);
            shard.hits.fetch_add(1, Ordering::Relaxed);
            (value, false)
        }
    }

    /// Non-renewing read: takes only a read lock, does not promote LRU recency, does not touch
    /// the hits/misses counters, and does not remove the entry. Returns `(Some(v), expired)` for
    /// a present entry (expired or not) or `(None, false)` when absent.
    fn cache_peek_with_expiry_status(&self, k: &K) -> (Option<V>, bool) {
        let shard = self.shard_of(k);
        let guard = shard.lock.read();
        match guard.cache_peek(k) {
            None => (None, false),
            Some(v) => {
                let expired = v.is_expired();
                (Some(v.clone()), expired)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ConcurrentCached as SyncConcurrentCached;
    use crate::ConcurrentCloneCached;

    #[derive(Clone)]
    struct Val {
        v: u32,
        expired: bool,
    }
    impl crate::Expires for Val {
        fn is_expired(&self) -> bool {
            self.expired
        }
    }

    #[test]
    fn new_returns_ready_cache_respecting_max_size() {
        // shards(1) gives an exact eviction bound.
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .shards(1)
            .max_size(2)
            .build()
            .unwrap();
        SyncConcurrentCached::set(
            &c,
            1,
            Val {
                v: 10,
                expired: false,
            },
        )
        .unwrap();
        assert_eq!(
            SyncConcurrentCached::get(&c, &1).unwrap().map(|v| v.v),
            Some(10)
        );
        SyncConcurrentCached::set(
            &c,
            2,
            Val {
                v: 20,
                expired: false,
            },
        )
        .unwrap();
        SyncConcurrentCached::set(
            &c,
            3,
            Val {
                v: 30,
                expired: false,
            },
        )
        .unwrap(); // evicts LRU (1)
        assert_eq!(c.len(), 2);
        assert!(SyncConcurrentCached::get(&c, &1).unwrap().is_none());

        // Inherent `new` returns a ready cache too.
        let c2 = ShardedExpiringLruCache::<u32, Val>::new(64);
        SyncConcurrentCached::set(
            &c2,
            1,
            Val {
                v: 1,
                expired: false,
            },
        )
        .unwrap();
        assert_eq!(
            SyncConcurrentCached::get(&c2, &1).unwrap().map(|v| v.v),
            Some(1)
        );

        // `new(N)` must forward N to the builder — capacity must equal the builder path.
        assert_eq!(
            ShardedExpiringLruCache::<u32, Val>::new(1024).capacity(),
            ShardedExpiringLruCache::<u32, Val>::builder()
                .max_size(1024)
                .build()
                .unwrap()
                .capacity()
        );
    }

    #[test]
    #[should_panic(expected = "non-zero max_size")]
    fn new_zero_max_size_panics() {
        let _c = ShardedExpiringLruCache::<u32, Val>::new(0);
    }

    #[test]
    fn copy_from_skips_expired() {
        let old = ShardedExpiringLruCache::<u32, Val>::builder()
            .max_size(64)
            .build()
            .unwrap();
        for i in 0..10u32 {
            SyncConcurrentCached::cache_set(
                &old,
                i,
                Val {
                    v: i,
                    expired: true,
                },
            )
            .expect("insert must succeed");
        }
        let new_cache = ShardedExpiringLruCacheBase::<u32, Val>::builder()
            .max_size(64)
            .copy_from(&old);
        assert_eq!(new_cache.len(), 0);
    }

    #[test]
    fn copy_from_preserves_live_entries() {
        let old = ShardedExpiringLruCache::<u32, Val>::builder()
            .max_size(64)
            .build()
            .unwrap();
        for i in 0..20u32 {
            SyncConcurrentCached::cache_set(
                &old,
                i,
                Val {
                    v: i * 10,
                    expired: false,
                },
            )
            .expect("insert must succeed");
        }
        let new_cache = ShardedExpiringLruCacheBase::<u32, Val>::builder()
            .max_size(64)
            .copy_from(&old);
        assert_eq!(new_cache.len(), 20);
        for i in 0..20u32 {
            let got =
                SyncConcurrentCached::cache_get(&new_cache, &i).expect("key was just inserted");
            assert_eq!(got.map(|v| v.v), Some(i * 10));
        }
    }

    #[test]
    fn copy_from_respects_capacity() {
        let old = ShardedExpiringLruCache::<u32, Val>::builder()
            .max_size(64)
            .build()
            .unwrap();
        for i in 0..40u32 {
            SyncConcurrentCached::cache_set(
                &old,
                i,
                Val {
                    v: i,
                    expired: false,
                },
            )
            .expect("insert must succeed");
        }
        let new_cache = ShardedExpiringLruCacheBase::<u32, Val>::builder()
            .shards(1)
            .max_size(8)
            .copy_from(&old);
        assert!(
            new_cache.len() <= 8,
            "new cache should not exceed capacity; got {}",
            new_cache.len()
        );
        assert!(!new_cache.is_empty(), "new cache should not be empty");
    }

    #[test]
    fn cache_remove_fires_on_evict_and_updates_metrics() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU64, Ordering as AtomicOrd};

        let evict_count = Arc::new(AtomicU64::new(0));
        let ec = evict_count.clone();
        let cache = ShardedExpiringLruCacheBase::<u32, Val>::builder()
            .shards(1)
            .max_size(8)
            .on_evict(move |_, _| {
                ec.fetch_add(1, AtomicOrd::Relaxed);
            })
            .build()
            .unwrap();

        SyncConcurrentCached::cache_set(
            &cache,
            1,
            Val {
                v: 1,
                expired: false,
            },
        )
        .expect("insert must succeed");
        SyncConcurrentCached::cache_set(
            &cache,
            2,
            Val {
                v: 2,
                expired: true,
            },
        )
        .expect("insert must succeed");

        let before = cache
            .metrics()
            .evictions
            .expect("eviction-tracking stores report an evictions count");

        // Removing a live (non-expired) entry fires on_evict and increments evictions.
        let got = SyncConcurrentCached::cache_remove(&cache, &1).expect("key must be present");
        assert_eq!(got.map(|v| v.v), Some(1));
        assert_eq!(
            evict_count.load(AtomicOrd::Relaxed),
            1,
            "on_evict must fire"
        );
        assert_eq!(
            cache
                .metrics()
                .evictions
                .expect("eviction-tracking stores report an evictions count")
                - before,
            1,
            "evictions metric must increment on successful remove"
        );

        // Removing an expired entry fires on_evict and increments evictions, but
        // returns None (the value is expired) — consistent across all stores.
        let before2 = cache
            .metrics()
            .evictions
            .expect("eviction-tracking stores report an evictions count");
        let got2 = SyncConcurrentCached::cache_remove(&cache, &2).expect("key must be present");
        assert_eq!(
            got2.map(|v| v.v),
            None,
            "expired entry must return None from cache_remove"
        );
        assert_eq!(
            evict_count.load(AtomicOrd::Relaxed),
            2,
            "on_evict must fire even for expired entries"
        );
        // Evictions counter still increments for expired explicit removes.
        assert_eq!(
            cache
                .metrics()
                .evictions
                .expect("eviction-tracking stores report an evictions count")
                - before2,
            1,
            "evictions metric increments even for expired removes"
        );
    }

    #[test]
    fn cache_clear_with_on_evict_fires_for_all_entries() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU64, Ordering as AtomicOrd};
        let count = Arc::new(AtomicU64::new(0));
        let count2 = count.clone();
        let c = ShardedExpiringLruCacheBase::<u32, Val>::builder()
            .shards(1)
            .max_size(64)
            .on_evict(move |_, _| {
                count2.fetch_add(1, AtomicOrd::Relaxed);
            })
            .build()
            .unwrap();
        for i in 0..20u32 {
            SyncConcurrentCached::cache_set(
                &c,
                i,
                Val {
                    v: i,
                    expired: false,
                },
            )
            .expect("insert must succeed");
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
            count.load(AtomicOrd::Relaxed),
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
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU64, Ordering as AtomicOrd};
        let count = Arc::new(AtomicU64::new(0));
        let count2 = count.clone();
        let c = ShardedExpiringLruCacheBase::<u32, Val>::builder()
            .max_size(64)
            .on_evict(move |_, _| {
                count2.fetch_add(1, AtomicOrd::Relaxed);
            })
            .build()
            .unwrap();
        for i in 0..10u32 {
            SyncConcurrentCached::cache_set(
                &c,
                i,
                Val {
                    v: i,
                    expired: false,
                },
            )
            .expect("insert must succeed");
        }
        c.clear();
        assert_eq!(
            count.load(AtomicOrd::Relaxed),
            0,
            "clear must not fire on_evict"
        );
    }

    #[test]
    fn cache_remove_entry_returns_some_for_live_entry() {
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .max_size(64)
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(
            &c,
            1u32,
            Val {
                v: 1,
                expired: false,
            },
        )
        .expect("insert must succeed");
        assert!(
            SyncConcurrentCached::cache_remove_entry(&c, &999u32)
                .expect("cache_remove_entry must succeed")
                .is_none()
        );
        let removed =
            SyncConcurrentCached::cache_remove_entry(&c, &1u32).expect("key must be present");
        assert!(removed.is_some());
        assert_eq!(removed.expect("must be Some").0, 1u32);
        assert!(
            SyncConcurrentCached::cache_get(&c, &1u32)
                .expect("cache_get must succeed")
                .is_none()
        );
    }

    #[test]
    fn cache_remove_entry_returns_some_for_expired_entry() {
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .max_size(64)
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(
            &c,
            1u32,
            Val {
                v: 1,
                expired: true,
            },
        )
        .expect("insert must succeed");
        SyncConcurrentCached::cache_set(
            &c,
            2u32,
            Val {
                v: 2,
                expired: true,
            },
        )
        .expect("insert must succeed");

        // cache_remove returns None for expired.
        assert!(
            SyncConcurrentCached::cache_remove(&c, &1u32)
                .expect("cache_remove must succeed")
                .is_none()
        );

        // cache_remove_entry returns Some even for expired.
        let removed =
            SyncConcurrentCached::cache_remove_entry(&c, &2u32).expect("key must be present");
        assert!(
            removed.is_some(),
            "cache_remove_entry must return Some for expired entry"
        );
        assert_eq!(removed.expect("must be Some").0, 2u32);
    }

    #[test]
    fn cache_delete_returns_true_for_expired_entry() {
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .max_size(64)
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(
            &c,
            1u32,
            Val {
                v: 1,
                expired: true,
            },
        )
        .expect("insert must succeed");
        assert!(
            SyncConcurrentCached::cache_delete(&c, &1u32).expect("cache_delete must succeed"),
            "cache_delete must be true for expired entry"
        );
        assert!(!SyncConcurrentCached::cache_delete(&c, &1u32).expect("cache_delete must succeed"));
    }

    #[test]
    fn cache_remove_entry_fires_on_evict_for_expired() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU64, Ordering as AtomicOrd};
        let count = Arc::new(AtomicU64::new(0));
        let count2 = count.clone();
        let c = ShardedExpiringLruCacheBase::<u32, Val>::builder()
            .shards(1)
            .max_size(64)
            .on_evict(move |_, _| {
                count2.fetch_add(1, AtomicOrd::Relaxed);
            })
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(
            &c,
            1u32,
            Val {
                v: 1,
                expired: true,
            },
        )
        .expect("insert must succeed");
        SyncConcurrentCached::cache_remove_entry(&c, &1u32).expect("key must be present");
        assert_eq!(
            count.load(AtomicOrd::Relaxed),
            1,
            "on_evict fires for expired entries"
        );

        SyncConcurrentCached::cache_remove_entry(&c, &999u32)
            .expect("cache_remove_entry must succeed");
        assert_eq!(count.load(AtomicOrd::Relaxed), 1, "no fire for absent key");
    }

    #[test]
    fn cache_remove_entry_increments_eviction_counter() {
        let c = ShardedExpiringLruCacheBase::<u32, Val>::builder()
            .max_size(64)
            .shards(1)
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(
            &c,
            1u32,
            Val {
                v: 1,
                expired: true,
            },
        )
        .expect("insert must succeed");
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
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .max_size(64)
            .build()
            .unwrap();
        let (val, expired) = ConcurrentCloneCached::cache_get_with_expiry_status(&c, &1u32);
        assert!(val.is_none(), "absent key must return None");
        assert!(!expired, "absent key must return expired=false");
        assert_eq!(
            c.metrics().misses,
            Some(1),
            "absent lookup must increment misses"
        );
    }

    #[test]
    fn concurrent_clone_cached_live_entry_is_some_false() {
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .max_size(64)
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(
            &c,
            1u32,
            Val {
                v: 7,
                expired: false,
            },
        )
        .expect("insert must succeed");
        let result = ConcurrentCloneCached::cache_get_with_expiry_status(&c, &1u32);
        assert_eq!(
            result.0.map(|v| v.v),
            Some(7),
            "live entry must return the value"
        );
        assert!(!result.1, "live entry must not set the expired flag");
        assert_eq!(c.metrics().hits, Some(1), "live lookup must increment hits");
        assert_eq!(
            c.metrics().evictions,
            Some(0),
            "live lookup must not increment evictions"
        );
    }

    #[test]
    fn concurrent_clone_cached_expired_returns_stale_no_eviction() {
        let c = ShardedExpiringLruCacheBase::<u32, Val>::builder()
            .max_size(64)
            .shards(1)
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(
            &c,
            1u32,
            Val {
                v: 55,
                expired: true,
            },
        )
        .expect("insert must succeed");

        let result = ConcurrentCloneCached::cache_get_with_expiry_status(&c, &1u32);
        assert_eq!(
            result.0.map(|v| v.v),
            Some(55),
            "expired entry must return the stale value"
        );
        assert!(result.1, "expired entry must set the expired flag");
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

        // Entry must NOT have been removed — a second call still sees it.
        let result2 = ConcurrentCloneCached::cache_get_with_expiry_status(&c, &1u32);
        assert_eq!(
            result2.0.map(|v| v.v),
            Some(55),
            "entry must still be present after expiry-status lookup"
        );
        assert!(
            result2.1,
            "entry must still be expired on second expiry-status call"
        );
    }

    #[test]
    fn peek_with_expiry_status_no_side_effects() {
        // shards(1) makes counter captures exact.
        let c = ShardedExpiringLruCacheBase::<u32, Val>::builder()
            .max_size(64)
            .shards(1)
            .build()
            .unwrap();

        SyncConcurrentCached::cache_set(
            &c,
            1u32,
            Val {
                v: 42,
                expired: false,
            },
        )
        .expect("insert must succeed");

        // Capture counters before any peek.
        let before = c.metrics();

        // Live key: expect (Some(v), false).
        let (val, expired) = ConcurrentCloneCached::cache_peek_with_expiry_status(&c, &1u32);
        assert_eq!(
            val.map(|x| x.v),
            Some(42),
            "live peek must return the value"
        );
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
        assert!(
            SyncConcurrentCached::cache_get(&c, &1u32)
                .expect("cache_get must succeed")
                .is_some(),
            "entry must still be present after peek"
        );
    }

    #[test]
    fn peek_with_expiry_status_does_not_promote_lru() {
        // max_size(2) + shards(1): a single shard with 2 slots. If peek promoted
        // recency, inserting a third entry would evict key 2 (MRU before peek);
        // if it does not promote, key 1 remains LRU and is evicted instead.
        let c = ShardedExpiringLruCacheBase::<u32, Val>::builder()
            .max_size(2)
            .shards(1)
            .build()
            .unwrap();

        // Insert order: key 1, then key 2. LRU is key 1.
        SyncConcurrentCached::cache_set(
            &c,
            1u32,
            Val {
                v: 10,
                expired: false,
            },
        )
        .expect("insert must succeed");
        SyncConcurrentCached::cache_set(
            &c,
            2u32,
            Val {
                v: 20,
                expired: false,
            },
        )
        .expect("insert must succeed");

        // Peek key 1 — must NOT promote it to MRU.
        let (val, expired) = ConcurrentCloneCached::cache_peek_with_expiry_status(&c, &1u32);
        assert_eq!(val.map(|x| x.v), Some(10), "peek must return the value");
        assert!(!expired, "peek must report expired=false");

        // Counters unchanged: no hits, no misses.
        let m = c.metrics();
        assert_eq!(m.hits, Some(0), "peek must not increment hits");
        assert_eq!(m.misses, Some(0), "peek must not increment misses");

        // Inserting key 3 must evict key 1 (still LRU), not key 2.
        SyncConcurrentCached::cache_set(
            &c,
            3u32,
            Val {
                v: 30,
                expired: false,
            },
        )
        .expect("insert must succeed");

        assert!(
            SyncConcurrentCached::cache_get(&c, &1u32)
                .expect("cache_get must succeed")
                .is_none(),
            "key 1 must be evicted as LRU (peek must not have promoted it)"
        );
        assert!(
            SyncConcurrentCached::cache_get(&c, &2u32)
                .expect("cache_get must succeed")
                .is_some(),
            "key 2 must survive"
        );
        assert!(
            SyncConcurrentCached::cache_get(&c, &3u32)
                .expect("cache_get must succeed")
                .is_some(),
            "key 3 must survive"
        );
    }

    #[test]
    fn peek_with_expiry_status_stale_entry_no_side_effects() {
        let c = ShardedExpiringLruCacheBase::<u32, Val>::builder()
            .max_size(64)
            .shards(1)
            .build()
            .unwrap();

        SyncConcurrentCached::cache_set(
            &c,
            1u32,
            Val {
                v: 77,
                expired: true,
            },
        )
        .expect("insert must succeed");

        let before = c.metrics();

        let (val, expired) = ConcurrentCloneCached::cache_peek_with_expiry_status(&c, &1u32);
        assert_eq!(
            val.map(|x| x.v),
            Some(77),
            "expired peek must return the stale value"
        );
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
            val2.map(|x| x.v),
            Some(77),
            "entry must still be present after expired peek"
        );
        assert!(expired2, "entry must still be expired after peek");
    }
}
