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
use crate::{CacheMetrics, ConcurrentCached, ConcurrentCloneCached, Expires};

use super::{
    CachePadded, DefaultShardHasher, Shard, ShardHasher, checked_shard_count, shard_index,
};
use crate::ConcurrentCacheEvict;
use crate::stores::BuildError;

type OnEvict<K, V> = Arc<dyn Fn(&K, &V) + Send + Sync>;

#[allow(clippy::type_complexity)]
struct ExpiringInner<K, V, H> {
    shards: Box<[CachePadded<Shard<HashMap<K, V, RandomState>>>]>,
    shard_mask: usize,
    hasher: H,
    on_evict: Option<OnEvict<K, V>>,
    evictions: AtomicU64,
}

/// A fully-concurrent, partitioned, unbounded in-memory cache with per-value expiry.
///
/// Each value controls its own expiration by implementing [`Expires`]. Expired entries
/// are checked on lookup and evicted on access or during explicit [`evict`](ConcurrentCacheEvict::evict) sweeps.
///
/// **Memory note:** This store is unbounded. Expired entries are only removed on access or
/// when [`evict`](ConcurrentCacheEvict::evict) is called explicitly. For high-cardinality workloads,
/// call `evict()` periodically or use [`ShardedExpiringLruCache`](crate::ShardedExpiringLruCache) with a `max_size` bound.
///
/// Wraps an `Arc` — `clone()` is an Arc-share (shared state), not a deep copy.
/// Use [`deep_clone`](ShardedExpiringCacheBase::deep_clone) to get an independent copy.
///
/// **Note**: reads return owned values cloned from under the shard lock, so `V` must
/// implement `Clone` (in addition to `Expires`).
///
/// This is a type alias for `ShardedExpiringCacheBase<K, V, DefaultShardHasher>`.
/// To use a custom shard hasher, call [`ShardedExpiringCache::builder()`] and then
/// [`hasher`](ShardedExpiringCacheBuilder::hasher), which yields a
/// `ShardedExpiringCacheBase<K, V, H>` over your hasher.
pub type ShardedExpiringCache<K, V> = ShardedExpiringCacheBase<K, V, DefaultShardHasher>;

/// Backing type for [`ShardedExpiringCache`] with a generic shard hasher `H`.
pub struct ShardedExpiringCacheBase<K, V, H = DefaultShardHasher> {
    inner: Arc<ExpiringInner<K, V, H>>,
}

impl<K, V, H> Clone for ShardedExpiringCacheBase<K, V, H> {
    /// Arc-share clone — both handles point to the same underlying cache.
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<K, V, H> std::fmt::Debug for ShardedExpiringCacheBase<K, V, H> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShardedExpiringCache")
            .field("shards", &self.inner.shards.len())
            .field("evictions", &self.inner.evictions.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl<K, V> ShardedExpiringCacheBase<K, V, DefaultShardHasher>
where
    K: Hash + Eq,
    V: Expires,
{
    /// Construct a ready-to-use [`ShardedExpiringCache`] with the [`DefaultShardHasher`]
    /// and a default shard count.
    ///
    /// `ShardedExpiringCache` has no required configuration, so this never fails. For a
    /// custom hasher, shard count, or `on_evict`, use [`builder`](Self::builder).
    #[must_use]
    pub fn new() -> ShardedExpiringCache<K, V> {
        Self::builder()
            .build()
            .expect("ShardedExpiringCache default build is infallible")
    }

    /// Return a builder for constructing a [`ShardedExpiringCache`].
    ///
    /// The builder starts with the [`DefaultShardHasher`]. To use a custom hasher, call
    /// [`hasher`](ShardedExpiringCacheBuilder::hasher) on the returned builder; it switches the
    /// builder's hasher type and `build` then yields a `ShardedExpiringCacheBase` over that
    /// hasher. `new` and `builder` exist only on the default-hasher alias, so a custom hasher is
    /// always introduced via `hasher`, never a `ShardedExpiringCacheBase::<_, _, H>` turbofish.
    #[must_use]
    pub fn builder() -> ShardedExpiringCacheBuilder<K, V, DefaultShardHasher> {
        ShardedExpiringCacheBuilder::default()
    }
}

impl<K, V, H> ShardedExpiringCacheBase<K, V, H>
where
    K: Hash + Eq,
    V: Expires,
    H: ShardHasher<K>,
{
    #[inline]
    fn shard_of(&self, k: &K) -> &CachePadded<Shard<HashMap<K, V, RandomState>>> {
        let h = self.inner.hasher.shard_hash(k);
        &self.inner.shards[shard_index(h, self.inner.shard_mask)]
    }
}

impl<K, V> Default for ShardedExpiringCache<K, V>
where
    K: Hash + Eq,
    V: Expires,
{
    fn default() -> Self {
        ShardedExpiringCacheBuilder::default()
            .build()
            .unwrap_or_else(|e| panic!("ShardedExpiringCache build failed: {e}"))
    }
}

impl<K: Clone + Hash + Eq, V: Clone + Expires, H: ShardHasher<K>>
    ShardedExpiringCacheBase<K, V, H>
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
            inner: Arc::new(ExpiringInner {
                shards,
                shard_mask: self.inner.shard_mask,
                hasher: self.inner.hasher.clone(),
                on_evict: self.inner.on_evict.clone(),
                evictions: AtomicU64::new(self.inner.evictions.load(Ordering::Relaxed)),
            }),
        }
    }
}

impl<K, V, H: ShardHasher<K>> ShardedExpiringCacheBase<K, V, H>
where
    K: Hash + Eq,
    V: Expires,
{
    /// Return aggregate metrics across all shards.
    ///
    /// `size` counts all stored entries, including expired ones that have not yet been
    /// swept by a call to [`evict`](ShardedExpiringCacheBase::evict).
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
            evictions: Some(self.inner.evictions.load(Ordering::Relaxed)),
            entry_count: size,
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
    /// If no `on_evict` callback is configured, this is equivalent to [`clear`](Self::clear).
    /// Increments the evictions counter for each removed entry only when `on_evict` is set.
    pub fn cache_clear_with_on_evict(&self) {
        if self.inner.on_evict.is_none() {
            return self.clear();
        }
        for shard in self.inner.shards.iter() {
            let removed: Vec<(K, V)> = shard.lock.write().drain().collect();
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
    }

    /// Sweep all shards for expired entries, remove them, fire the `on_evict` callback
    /// (if set) for each, and return the total count of removed entries.
    #[must_use]
    pub fn evict(&self) -> usize
    where
        K: Clone,
    {
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
                    if let Some((key, v)) = guard.remove_entry(&k) {
                        removed.push((key, v));
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

impl<K, V, H> ConcurrentCached<K, V> for ShardedExpiringCacheBase<K, V, H>
where
    K: Hash + Eq,
    V: Clone + Expires,
    H: ShardHasher<K>,
{
    type Error = std::convert::Infallible;

    fn cache_get(&self, k: &K) -> Result<Option<V>, Self::Error> {
        let shard = self.shard_of(k);
        // Expiry check — try with a read lock first to allow read concurrency on hits.
        let (expired, value) = {
            let guard = shard.lock.read();
            match guard.get(k) {
                Some(v) => {
                    let expired = v.is_expired();
                    let val = if !expired { Some(v.clone()) } else { None };
                    (expired, val)
                }
                None => {
                    shard.misses.fetch_add(1, Ordering::Relaxed);
                    return Ok(None);
                }
            }
        };

        if expired {
            // Upgrade to write lock to remove the expired entry.
            let mut guard = shard.lock.write();
            // Re-check under write lock — another thread may have replaced the entry
            // with a fresh value in the meantime; clone it out in the same lookup.
            let fresh_val = match guard.get(k) {
                Some(v) if !v.is_expired() => Some(v.clone()),
                _ => None,
            };
            if let Some(fresh_val) = fresh_val {
                drop(guard);
                shard.hits.fetch_add(1, Ordering::Relaxed);
                return Ok(Some(fresh_val));
            }
            // Still expired (or already gone) — remove it.
            let removed = guard.remove_entry(k);
            drop(guard);
            if let Some((stored_k, v)) = removed {
                self.inner.evictions.fetch_add(1, Ordering::Relaxed);
                if let Some(on_evict) = &self.inner.on_evict {
                    on_evict(&stored_k, &v);
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
        Ok(shard.lock.write().insert(k, v))
    }

    fn cache_remove(&self, k: &K) -> Result<Option<V>, Self::Error> {
        let shard = self.shard_of(k);
        let removed = shard.lock.write().remove_entry(k);
        if let Some((stored_k, v)) = removed {
            self.inner.evictions.fetch_add(1, Ordering::Relaxed);
            if let Some(on_evict) = &self.inner.on_evict {
                on_evict(&stored_k, &v);
            }
            if v.is_expired() {
                Ok(None)
            } else {
                Ok(Some(v))
            }
        } else {
            Ok(None)
        }
    }

    fn cache_remove_entry(&self, k: &K) -> Result<Option<(K, V)>, Self::Error> {
        let shard = self.shard_of(k);
        let removed = shard.lock.write().remove_entry(k);
        if let Some((ref stored_k, ref v)) = removed {
            self.inner.evictions.fetch_add(1, Ordering::Relaxed);
            if let Some(on_evict) = &self.inner.on_evict {
                on_evict(stored_k, v);
            }
        }
        Ok(removed)
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
impl<K, V, H> ConcurrentCachedAsync<K, V> for ShardedExpiringCacheBase<K, V, H>
where
    K: Hash + Eq + Send + Sync,
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

impl<K, V, H> ConcurrentCacheEvict for ShardedExpiringCacheBase<K, V, H>
where
    K: Clone + Hash + Eq,
    V: Expires,
    H: ShardHasher<K>,
{
    fn evict(&self) -> usize {
        ShardedExpiringCacheBase::evict(self)
    }
}

/// Builder for [`ShardedExpiringCacheBase`].
///
/// Note: there is intentionally **no `.ttl()` setter**. A sharded expiring cache has no global
/// expiry duration — each value decides when it is expired via the [`Expires`] trait. For a
/// single global TTL applied to every entry, use
/// [`ShardedTtlCache`](crate::ShardedTtlCache) or
/// [`ShardedLruTtlCache`](crate::ShardedLruTtlCache) instead.
#[doc(alias = "ttl")]
pub struct ShardedExpiringCacheBuilder<K, V: Expires, H = DefaultShardHasher> {
    shards: Option<usize>,
    hasher: Option<H>,
    on_evict: Option<OnEvict<K, V>>,
    _k: std::marker::PhantomData<K>,
    _v: std::marker::PhantomData<V>,
}

impl<K, V: Expires> Default for ShardedExpiringCacheBuilder<K, V, DefaultShardHasher> {
    fn default() -> Self {
        Self {
            shards: None,
            hasher: Some(DefaultShardHasher::default()),
            on_evict: None,
            _k: std::marker::PhantomData,
            _v: std::marker::PhantomData,
        }
    }
}

impl<K, V: Expires, H> ShardedExpiringCacheBuilder<K, V, H> {
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
    pub fn hasher<H2: ShardHasher<K>>(self, hasher: H2) -> ShardedExpiringCacheBuilder<K, V, H2> {
        ShardedExpiringCacheBuilder {
            shards: self.shards,
            hasher: Some(hasher),
            on_evict: self.on_evict,
            _k: std::marker::PhantomData,
            _v: std::marker::PhantomData,
        }
    }

    /// Set a callback invoked when an entry is evicted. Fires on expired-entry removal during
    /// [`cache_get`](ConcurrentCached::cache_get), explicitly via
    /// [`evict`](ShardedExpiringCacheBase::evict), on explicit
    /// [`cache_remove`](ConcurrentCached::cache_remove), and on
    /// [`cache_remove_entry`](ConcurrentCached::cache_remove_entry).
    /// Does **not** fire on [`clear`](ShardedExpiringCacheBase::clear);
    /// use [`cache_clear_with_on_evict`](ShardedExpiringCacheBase::cache_clear_with_on_evict) to opt in.
    ///
    /// The closure must be `'static` (its captures cannot borrow from the local stack), but `K`
    /// and `V` themselves are not required to be `'static`.
    #[must_use]
    pub fn on_evict(mut self, on_evict: impl Fn(&K, &V) + Send + Sync + 'static) -> Self {
        self.on_evict = Some(Arc::new(on_evict));
        self
    }

    /// Build the new cache and copy every non-expired entry from `existing` into it.
    ///
    /// Acquires each shard's read lock on `existing` one at a time — `existing`
    /// keeps serving concurrent ops throughout. Entries whose
    /// [`is_expired`](crate::Expires::is_expired) returns `true` at copy time are
    /// skipped and not transferred.
    ///
    /// **Note**: `on_evict` callbacks on `existing` do not fire — entries are read
    /// (not removed) from the source cache.
    #[must_use]
    pub fn copy_from<H2: ShardHasher<K>>(
        self,
        existing: &ShardedExpiringCacheBase<K, V, H2>,
    ) -> ShardedExpiringCacheBase<K, V, H>
    where
        K: Clone + Hash + Eq,
        V: Clone,
        H: ShardHasher<K>,
    {
        let new_cache = self
            .build()
            .unwrap_or_else(|e| panic!("ShardedExpiringCache build failed: {e}"));
        for shard in existing.inner.shards.iter() {
            let entries: Vec<(K, V)> = {
                let guard = shard.lock.read();
                guard
                    .iter()
                    .filter(|(_, v)| !v.is_expired())
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect()
            };
            for (k, v) in entries {
                let _ = ConcurrentCached::cache_set(&new_cache, k, v);
            }
        }
        new_cache
    }

    /// Build the cache.
    ///
    /// Use [`ShardedExpiringCache::builder()`] (or [`ShardedExpiringCacheBase::builder()`]) to
    /// obtain a builder, configure it, then call `.build()`.
    ///
    /// This builder never fails for valid inputs. Returns `Ok(cache)` on success.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError`] if the `shards` count is zero or overflows when rounded
    /// up to the next power of two.
    #[must_use = "the Result from build() must be used"]
    pub fn build(self) -> Result<ShardedExpiringCacheBase<K, V, H>, BuildError>
    where
        K: Hash + Eq,
        H: ShardHasher<K>,
    {
        let n = checked_shard_count(self.shards)?;
        let mask = n - 1;
        let shards = (0..n)
            .map(|_| CachePadded(Shard::new(HashMap::with_hasher(RandomState::new()))))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Ok(ShardedExpiringCacheBase {
            inner: Arc::new(ExpiringInner {
                shards,
                shard_mask: mask,
                hasher: self
                    .hasher
                    .expect("hasher is always initialized via Default or .hasher()"),
                on_evict: self.on_evict,
                evictions: AtomicU64::new(0),
            }),
        })
    }
}

impl<K, V, H> ConcurrentCloneCached<K, V> for ShardedExpiringCacheBase<K, V, H>
where
    K: Hash + Eq,
    V: Clone + Expires,
    H: ShardHasher<K>,
{
    /// Returns `(Some(v), false)` for a live entry (hit), `(Some(v), true)` for an expired
    /// entry (miss, **no removal**, no eviction counter), or `(None, false)` when absent (miss).
    fn cache_get_with_expiry_status(&self, k: &K) -> (Option<V>, bool) {
        let shard = self.shard_of(k);
        let guard = shard.lock.read();
        match guard.get(k) {
            None => {
                drop(guard);
                shard.misses.fetch_add(1, Ordering::Relaxed);
                (None, false)
            }
            Some(v) => {
                let expired = v.is_expired();
                let value = v.clone();
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

    /// Non-renewing read: takes only a read lock, never touches the hits/misses counters or
    /// removes the entry. Returns `(Some(v), expired)` for a present entry (expired or not) or
    /// `(None, false)` when absent.
    fn cache_peek_with_expiry_status(&self, k: &K) -> (Option<V>, bool) {
        let shard = self.shard_of(k);
        let guard = shard.lock.read();
        match guard.get(k) {
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
    fn new_returns_ready_cache() {
        let c = ShardedExpiringCache::<u32, Val>::new();
        assert_eq!(
            SyncConcurrentCached::set(
                &c,
                1,
                Val {
                    v: 10,
                    expired: false
                }
            )
            .unwrap()
            .map(|v| v.v),
            None
        );
        assert_eq!(
            SyncConcurrentCached::get(&c, &1).unwrap().map(|v| v.v),
            Some(10)
        );
        // Expired values are not returned.
        SyncConcurrentCached::set(
            &c,
            2,
            Val {
                v: 20,
                expired: true,
            },
        )
        .unwrap();
        assert!(SyncConcurrentCached::get(&c, &2).unwrap().is_none());
    }

    #[test]
    fn copy_from_skips_expired() {
        let old = ShardedExpiringCache::<u32, Val>::builder().build().unwrap();
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
        let new_cache = ShardedExpiringCacheBase::<u32, Val>::builder().copy_from(&old);
        assert_eq!(new_cache.len(), 0);
    }

    #[test]
    fn copy_from_preserves_live_entries() {
        let old = ShardedExpiringCache::<u32, Val>::builder().build().unwrap();
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
        let new_cache = ShardedExpiringCacheBase::<u32, Val>::builder().copy_from(&old);
        assert_eq!(new_cache.len(), 20);
        for i in 0..20u32 {
            let got =
                SyncConcurrentCached::cache_get(&new_cache, &i).expect("key was just inserted");
            assert_eq!(got.map(|v| v.v), Some(i * 10));
        }
    }

    #[test]
    fn cache_remove_fires_on_evict_and_updates_metrics() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU64, Ordering as AtomicOrd};

        let evict_count = Arc::new(AtomicU64::new(0));
        let ec = evict_count.clone();
        let cache = ShardedExpiringCacheBase::<u32, Val>::builder()
            .shards(1)
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

        // Removing a live entry fires on_evict and increments evictions.
        let before = cache
            .metrics()
            .evictions
            .expect("eviction-tracking stores report an evictions count");
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

        // Removing an expired entry fires on_evict and increments the evictions
        // counter, but returns None (the value is expired). This is consistent
        // across all stores: cache_remove returns None for an expired entry.
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
    fn build_returns_err_for_zero_shards() {
        let result = ShardedExpiringCacheBase::<u32, Val>::builder()
            .shards(0)
            .build();
        assert!(result.is_err(), "zero shards must return Err");
        let err = result.unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("shards"),
            "error must mention shards: {message}"
        );
    }

    #[test]
    fn cache_clear_with_on_evict_fires_for_all_entries() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU64, Ordering};
        let count = Arc::new(AtomicU64::new(0));
        let count2 = count.clone();
        let c = ShardedExpiringCacheBase::<u32, Val>::builder()
            .on_evict(move |_, _| {
                count2.fetch_add(1, Ordering::Relaxed);
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
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU64, Ordering};
        let count = Arc::new(AtomicU64::new(0));
        let count2 = count.clone();
        let c = ShardedExpiringCacheBase::<u32, Val>::builder()
            .on_evict(move |_, _| {
                count2.fetch_add(1, Ordering::Relaxed);
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
            count.load(Ordering::Relaxed),
            0,
            "clear must not fire on_evict"
        );
    }

    #[test]
    fn cache_remove_entry_returns_some_for_live_entry() {
        let c = ShardedExpiringCache::<u32, Val>::builder().build().unwrap();
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
        let c = ShardedExpiringCache::<u32, Val>::builder().build().unwrap();
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
        let c = ShardedExpiringCache::<u32, Val>::builder().build().unwrap();
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
        use std::sync::atomic::{AtomicU64, Ordering};
        let count = Arc::new(AtomicU64::new(0));
        let count2 = count.clone();
        let c = ShardedExpiringCacheBase::<u32, Val>::builder()
            .shards(1)
            .on_evict(move |_, _| {
                count2.fetch_add(1, Ordering::Relaxed);
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
        let c = ShardedExpiringCacheBase::<u32, Val>::builder()
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
        let c = ShardedExpiringCache::<u32, Val>::builder().build().unwrap();
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
        let c = ShardedExpiringCache::<u32, Val>::builder().build().unwrap();
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
        let c = ShardedExpiringCacheBase::<u32, Val>::builder()
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
        let c = ShardedExpiringCacheBase::<u32, Val>::builder()
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
    fn peek_with_expiry_status_stale_entry_no_side_effects() {
        // Use Val with expired=true to simulate a stale entry without sleeping.
        let c = ShardedExpiringCacheBase::<u32, Val>::builder()
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
