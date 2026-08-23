use std::hash::Hash;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(feature = "ahash")]
use ahash::RandomState;
#[cfg(not(feature = "ahash"))]
use std::collections::hash_map::RandomState;

use std::collections::HashMap;

use crate::{
    CacheMetrics, ConcurrentCacheBase, ConcurrentCacheExpiry, ConcurrentCachePeek,
    ConcurrentCached, ConcurrentCloneCached, Expires,
};
#[cfg(feature = "async_core")]
use crate::{ConcurrentCachePeekAsync, ConcurrentCachedAsync};
#[cfg(feature = "async_core")]
use core::future::Future;

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
/// Use [`deep_clone`](ShardedExpiringCache::deep_clone) to get an independent copy.
///
/// **Note**: reads return owned values cloned from under the shard lock, so `V` must
/// implement `Clone` (in addition to `Expires`).
///
/// **`len` / `evict` contract**: `len()` (the inherent method) returns the raw stored entry
/// count across all shards and may include expired-but-not-yet-swept entries. Call `evict()`
/// (via [`ConcurrentCacheEvict`](crate::ConcurrentCacheEvict)) to physically remove expired
/// entries, reclaim memory, and obtain an accurate live count. Sharded stores do not implement
/// `CachedIter`.
///
/// The shard-selection hasher `H` defaults to [`DefaultShardHasher`] (ahash-backed when the
/// `ahash` feature is enabled, otherwise `std::collections::hash_map::RandomState`), so
/// `ShardedExpiringCache<K, V>` names the common case. To use a custom [`ShardHasher`], call
/// [`ShardedExpiringCache::builder()`] and then
/// [`hasher`](ShardedExpiringCacheBuilder::hasher), which switches `H` to your hasher.
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
pub struct ShardedExpiringCache<K, V, H = DefaultShardHasher> {
    inner: Arc<ExpiringInner<K, V, H>>,
}

impl<K, V, H> Clone for ShardedExpiringCache<K, V, H> {
    /// Arc-share clone — both handles point to the same underlying cache.
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<K, V, H> std::fmt::Debug for ShardedExpiringCache<K, V, H> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let evictions: u64 = self
            .inner
            .shards
            .iter()
            .map(|s| s.evictions.load(Ordering::Relaxed))
            .sum();
        f.debug_struct("ShardedExpiringCache")
            .field("shards", &self.inner.shards.len())
            .field("evictions", &evictions)
            .finish_non_exhaustive()
    }
}

impl<K, V> ShardedExpiringCache<K, V, DefaultShardHasher>
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
    /// builder's hasher type and `build` then yields a `ShardedExpiringCache<K, V, H>` over that
    /// hasher. `new` and `builder` exist only on the default-hasher instantiation
    /// `ShardedExpiringCache<K, V, DefaultShardHasher>`, so a custom hasher is always introduced
    /// via `hasher`, never a `ShardedExpiringCache::<_, _, H>` turbofish.
    #[must_use]
    pub fn builder() -> ShardedExpiringCacheBuilder<K, V, DefaultShardHasher> {
        ShardedExpiringCacheBuilder::default()
    }
}

impl<K, V, H> ShardedExpiringCache<K, V, H>
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

impl<K: Clone + Hash + Eq, V: Clone + Expires, H: ShardHasher<K>> ShardedExpiringCache<K, V, H> {
    /// Return an independent deep copy of this cache — entries and metrics are
    /// duplicated, not shared. In most cases [`Clone::clone`] (Arc-share) is
    /// what you want.
    #[must_use]
    pub fn deep_clone(&self) -> Self {
        let n = self.inner.shards.len();
        let shards = (0..n)
            .map(|i| {
                // Load the hit/miss/eviction counters under the read lock so the metrics
                // snapshot is consistent with the entry snapshot (B4: loading after
                // drop(guard) could yield counters newer than the cloned entries).
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
            inner: Arc::new(ExpiringInner {
                shards,
                shard_mask: self.inner.shard_mask,
                hasher: self.inner.hasher.clone(),
                on_evict: self.inner.on_evict.clone(),
            }),
        }
    }
}

impl<K, V, H: ShardHasher<K>> ShardedExpiringCache<K, V, H>
where
    K: Hash + Eq,
    V: Clone + Expires,
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
    /// observable side effects: no hit/miss metrics, no lazy removal of an expired
    /// entry. The single-owner counterpart is
    /// [`CachedPeek::cache_peek`](crate::CachedPeek::cache_peek); the sharded stores
    /// return a clone rather than a reference because the value lives behind a
    /// per-shard lock.
    #[must_use]
    pub fn peek(&self, k: &K) -> Option<V> {
        let shard = self.shard_of(k);
        let guard = shard.lock.read();
        guard.get(k).filter(|v| !v.is_expired()).cloned()
    }
}

impl<K, V, H: ShardHasher<K>> ShardedExpiringCache<K, V, H>
where
    K: Hash + Eq,
    V: Expires,
{
    /// Return aggregate metrics across all shards.
    ///
    /// `size` counts all stored entries, including expired ones that have not yet been
    /// swept by a call to [`evict`](ShardedExpiringCache::evict).
    #[must_use]
    pub fn metrics(&self) -> CacheMetrics {
        let mut hits = 0u64;
        let mut misses = 0u64;
        let mut evictions = 0u64;
        let mut size = 0usize;
        for shard in self.inner.shards.iter() {
            hits += shard.hits.load(Ordering::Relaxed);
            misses += shard.misses.load(Ordering::Relaxed);
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
        if self.inner.on_evict.is_none() {
            for shard in self.inner.shards.iter() {
                let mut guard = shard.lock.write();
                let n = guard.len();
                guard.clear();
                drop(guard);
                if n > 0 {
                    shard.evictions.fetch_add(n as u64, Ordering::Relaxed);
                }
            }
            return;
        }
        for shard in self.inner.shards.iter() {
            let removed: Vec<(K, V)> = shard.lock.write().drain().collect();
            if !removed.is_empty() {
                shard
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
    ///
    /// # Panicking `is_expired`
    ///
    /// [`Expires::is_expired`] is user code. If it panics, nothing has been removed yet from
    /// the shard it panicked in (or from any shard not yet visited): each shard is swept in a
    /// first pass that only *selects* expired entries and a second pass that removes them
    /// without running user code. Shards already swept keep their removals, all of which were
    /// counted and notified before the panic. This holds whether or not an `on_evict` callback
    /// is configured.
    ///
    /// The `K: Clone` bound is not needed by this implementation; it predates the two-phase
    /// sweep and is kept because removing it from the inherent method alone would leave it
    /// stricter on the [`ConcurrentCacheEvict`](crate::ConcurrentCacheEvict) impl.
    #[must_use]
    pub fn evict(&self) -> usize
    where
        K: Clone,
    {
        let mut total = 0;
        for shard in self.inner.shards.iter() {
            // Collect under the write lock, fire callbacks after releasing it. Two phases: the
            // first calls `is_expired` (user code) and only selects, the second removes and
            // runs nothing that can panic. See `stores::take_doomed`. The no-callback path used
            // to take a `before - guard.len()` delta after an in-place `HashMap::retain`, which
            // a panicking `is_expired` skipped entirely; both paths now remove and count
            // exactly the same entries.
            let removed: Vec<(K, V)> = {
                let mut guard = shard.lock.write();
                crate::stores::take_doomed(&mut guard, |_, v| v.is_expired())
            };

            total += removed.len();
            if !removed.is_empty() {
                shard
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

    /// Retain only entries that are unexpired and satisfy `keep`.
    ///
    /// Removes every entry whose value reports [`is_expired`](Expires::is_expired) **or** for
    /// which `keep` returns `false` — expired entries are removed without consulting `keep`.
    /// `on_evict` is called and the eviction counter (`metrics().evictions`) incremented for
    /// each removed entry. The single-owner counterpart is
    /// [`ExpiringCache::retain`](crate::ExpiringCache::retain). This matches
    /// [`ShardedTtlCache::retain`](crate::ShardedTtlCache::retain); the plain
    /// [`ShardedUnboundCache::retain`](crate::ShardedUnboundCache::retain) has no expiry
    /// dimension and removes solely on the predicate.
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
    ///
    /// Returns the total number of entries removed across all shards for this call, folding
    /// together predicate-rejected entries and entries swept for having already expired -- the
    /// two are not distinguished in the count. Not `#[must_use]`: discarding the count is a
    /// legitimate and common use.
    ///
    /// # Panicking predicate
    ///
    /// If `keep` (or [`Expires::is_expired`], equally user code) panics, nothing has been
    /// removed yet from the shard it panicked in (or from any shard not yet visited): the sweep
    /// of a shard runs the predicates in a first pass that only *selects* doomed entries and
    /// removes them in a second pass that runs no user code. Shards already swept keep their
    /// removals, all of which were counted and notified before the panic. This holds whether or
    /// not an `on_evict` callback is configured.
    pub fn retain<F: FnMut(&K, &V) -> bool>(&self, mut keep: F) -> usize {
        let mut total_removed = 0usize;
        for shard in self.inner.shards.iter() {
            // Collect under the write lock, fire callbacks after releasing it. Two phases for
            // the same reason as `evict`; see `stores::take_doomed`. The no-callback path used
            // to take a `before - guard.len()` delta that a panicking predicate skipped
            // entirely; it now shares this structure.
            let removed: Vec<(K, V)> = {
                let mut guard = shard.lock.write();
                crate::stores::take_doomed(&mut guard, |k, v| v.is_expired() || !keep(k, v))
            };
            total_removed += removed.len();
            if !removed.is_empty() {
                shard
                    .evictions
                    .fetch_add(removed.len() as u64, Ordering::Relaxed);
                if let Some(on_evict) = &self.inner.on_evict {
                    for (k, v) in &removed {
                        on_evict(k, v);
                    }
                }
            }
        }
        total_removed
    }
}

impl<K, V, H> ConcurrentCacheBase for ShardedExpiringCache<K, V, H>
where
    K: Hash + Eq,
    V: Clone + Expires,
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

impl<K, V, H> ConcurrentCached<K, V> for ShardedExpiringCache<K, V, H>
where
    K: Hash + Eq,
    V: Clone + Expires,
    H: ShardHasher<K>,
{
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
                    drop(guard);
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
                shard.evictions.fetch_add(1, Ordering::Relaxed);
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
        // Capture the displaced value and evaluate is_expired() while the write lock is still
        // held (B2: avoids a TOCTOU where an entry crosses the expiry threshold between unlock
        // and the check). An owned key is kept in hand so `on_evict` can fire after the lock is
        // released (on_evict-after-unlock).
        //
        // There is exactly ONE write shape here, taken whether or not an `on_evict` callback is
        // configured, so attaching a purely observational callback cannot change which key is
        // physically stored. An overwrite goes through `remove_entry` + `insert`, rebinding the
        // slot to the CALLER's key (matching the LRU-backed sharded stores) and yielding the
        // displaced stored key owned, so `on_evict` receives the evicted entry's own (key,
        // value) pair after the lock is released. Every sharded `on_evict` site hands the
        // callback the stored pair; a `get_mut` value swap would keep the stored key in the
        // map but leave only the caller's (`Eq`-equal, possibly non-identical) key on hand
        // for the callback. Note the single-owner `ExpiringCache` differs on the stored-key
        // axis: it keeps the first-inserted key, matching `HashMap::insert`.
        let old: Option<(K, V, bool)> = {
            let mut guard = shard.lock.write();
            match guard.remove_entry(&k) {
                Some((stored_k, old_v)) => {
                    let expired = old_v.is_expired();
                    guard.insert(k, v);
                    Some((stored_k, old_v, expired))
                }
                None => {
                    guard.insert(k, v);
                    None
                }
            }
        };
        match old {
            // A displaced expired value is filtered from the return (matching cache_remove and
            // the single-owner expiring stores); fire on_evict and count an eviction for it.
            Some((key, old_v, true)) => {
                // Count BEFORE notifying: a panicking callback must never leave an
                // entry removed-but-uncounted.
                shard.evictions.fetch_add(1, Ordering::Relaxed);
                if let Some(cb) = &self.inner.on_evict {
                    cb(&key, &old_v);
                }
                Ok(None)
            }
            Some((_, old_v, false)) => Ok(Some(old_v)),
            None => Ok(None),
        }
    }

    /// Removes the entry and returns the value only if it is still live;
    /// an expired value is removed but reported as `Ok(None)`. Use
    /// [`cache_remove_entry`](ConcurrentCached::cache_remove_entry) to
    /// receive the value regardless of expiry.
    fn cache_remove(&self, k: &K) -> Result<Option<V>, Self::Error> {
        let shard = self.shard_of(k);
        let removed = shard.lock.write().remove_entry(k);
        if let Some((stored_k, v)) = removed {
            shard.evictions.fetch_add(1, Ordering::Relaxed);
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

    /// Removes the entry and returns it **regardless of expiry** (unlike
    /// [`cache_remove`](ConcurrentCached::cache_remove), which filters
    /// expired values).
    fn cache_remove_entry(&self, k: &K) -> Result<Option<(K, V)>, Self::Error> {
        let shard = self.shard_of(k);
        let removed = shard.lock.write().remove_entry(k);
        if let Some((ref stored_k, ref v)) = removed {
            shard.evictions.fetch_add(1, Ordering::Relaxed);
            if let Some(on_evict) = &self.inner.on_evict {
                on_evict(stored_k, v);
            }
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
            shard.evictions.store(0, Ordering::Relaxed);
        }
        Ok(())
    }

    /// Efficient peek-based contains: acquires a read lock, does not clone the value,
    /// and does not record hit/miss metrics. Returns `true` only for live (not expired) entries.
    fn cache_contains(&self, k: &K) -> Result<bool, Self::Error> {
        let shard = self.shard_of(k);
        Ok(shard.lock.read().get(k).is_some_and(|v| !v.is_expired()))
    }
}

impl<K, V, H> ConcurrentCachePeek<K, V> for ShardedExpiringCache<K, V, H>
where
    K: Hash + Eq,
    V: Clone + Expires,
    H: ShardHasher<K>,
{
    fn cache_peek(&self, k: &K) -> Result<Option<V>, Self::Error> {
        Ok(self.peek(k))
    }
}

#[cfg(feature = "async_core")]
#[cfg_attr(docsrs, doc(cfg(feature = "async_core")))]
impl<K, V, H> ConcurrentCachePeekAsync<K, V> for ShardedExpiringCache<K, V, H>
where
    K: Hash + Eq + Send + Sync,
    V: Clone + Expires + Send + Sync,
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
impl<K, V, H> ConcurrentCachedAsync<K, V> for ShardedExpiringCache<K, V, H>
where
    K: Hash + Eq + Send + Sync,
    V: Clone + Expires + Send + Sync,
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

    /// Efficient peek-based contains: does not clone the value, does not record hit/miss metrics,
    /// and returns `true` only for live (not expired) entries.
    fn async_cache_contains(&self, k: &K) -> impl Future<Output = Result<bool, Self::Error>> + Send
    where
        Self: Sized + Sync,
        K: Sync,
    {
        let result = ConcurrentCached::cache_contains(self, k);
        async move { result }
    }
}

impl<K, V, H> ConcurrentCacheEvict for ShardedExpiringCache<K, V, H>
where
    K: Clone + Hash + Eq,
    V: Expires,
    H: ShardHasher<K>,
{
    fn evict(&self) -> usize {
        ShardedExpiringCache::evict(self)
    }
}

/// Builder for [`ShardedExpiringCache`].
///
/// Note: there is intentionally **no `.ttl()` setter**. A sharded expiring cache has no global
/// expiry duration — each value decides when it is expired via the [`Expires`] trait. For a
/// single global TTL applied to every entry, use
/// [`ShardedTtlCache`](crate::ShardedTtlCache) or
/// [`ShardedLruTtlCache`](crate::ShardedLruTtlCache) instead.
#[doc(alias = "ttl")]
pub struct ShardedExpiringCacheBuilder<K, V, H = DefaultShardHasher> {
    shards: Option<usize>,
    per_shard_initial_capacity: Option<usize>,
    hasher: Option<H>,
    on_evict: Option<OnEvict<K, V>>,
    _k: std::marker::PhantomData<K>,
    _v: std::marker::PhantomData<V>,
}

impl<K, V> Default for ShardedExpiringCacheBuilder<K, V, DefaultShardHasher> {
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

impl<K, V> ShardedExpiringCacheBuilder<K, V> {
    /// Create a builder with default settings. Equivalent to [`ShardedExpiringCache::builder`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl<K, V, H> ShardedExpiringCacheBuilder<K, V, H> {
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
    pub fn hasher<H2: ShardHasher<K>>(self, hasher: H2) -> ShardedExpiringCacheBuilder<K, V, H2> {
        ShardedExpiringCacheBuilder {
            shards: self.shards,
            per_shard_initial_capacity: self.per_shard_initial_capacity,
            hasher: Some(hasher),
            on_evict: self.on_evict,
            _k: std::marker::PhantomData,
            _v: std::marker::PhantomData,
        }
    }

    /// Set a callback invoked when an entry is evicted. Fires in five situations:
    /// on expired-entry removal during [`cache_get`](ConcurrentCached::cache_get);
    /// explicitly via [`evict`](ShardedExpiringCache::evict); on explicit
    /// [`cache_remove`](ConcurrentCached::cache_remove); on
    /// [`cache_remove_entry`](ConcurrentCached::cache_remove_entry); and on
    /// [`cache_set`](ConcurrentCached::cache_set) when the displaced entry is already expired.
    /// Does **not** fire on [`clear`](ShardedExpiringCache::clear);
    /// use [`cache_clear_with_on_evict`](ShardedExpiringCache::cache_clear_with_on_evict) to opt in.
    /// [`cache_clear_with_on_evict`](ShardedExpiringCache::cache_clear_with_on_evict) fires
    /// callbacks after releasing the shard lock.
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
    ///
    /// # Errors
    ///
    /// Returns [`Err(BuildError)`](crate::stores::BuildError) if the builder
    /// configuration is invalid (the same conditions as [`build`](Self::build)).
    #[must_use = "the Result from copy_from() must be used"]
    pub fn copy_from<H2: ShardHasher<K>>(
        self,
        existing: &ShardedExpiringCache<K, V, H2>,
    ) -> Result<ShardedExpiringCache<K, V, H>, BuildError>
    where
        K: Clone + Hash + Eq,
        V: Clone + Expires,
        H: ShardHasher<K>,
    {
        let new_cache = self.build()?;
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
        Ok(new_cache)
    }

    /// Build the cache.
    ///
    /// Use [`ShardedExpiringCache::builder()`] to obtain a builder, configure it, then call
    /// `.build()`.
    ///
    /// This builder never fails for valid inputs. Returns `Ok(cache)` on success.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError`] if the `shards` count is zero or overflows when rounded
    /// up to the next power of two.
    #[must_use = "the Result from build() must be used"]
    pub fn build(self) -> Result<ShardedExpiringCache<K, V, H>, BuildError>
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
        Ok(ShardedExpiringCache {
            inner: Arc::new(ExpiringInner {
                shards,
                shard_mask: mask,
                hasher: self
                    .hasher
                    .expect("hasher is always initialized via Default or .hasher()"),
                on_evict: self.on_evict,
            }),
        })
    }
}

impl<K, V, H> ConcurrentCloneCached<K, V> for ShardedExpiringCache<K, V, H>
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

impl<K, V, H> ConcurrentCacheExpiry<K, V> for ShardedExpiringCache<K, V, H>
where
    K: Hash + Eq,
    V: Clone + Expires,
    H: ShardHasher<K>,
{
    /// Returns the stored value and its expiry instant, with no read side effects.
    ///
    /// Takes only a read lock. The instant comes from [`Expires::expires_at`], which is
    /// **advisory only** on this store, not authoritative: the default implementation returns
    /// `None`, so it is `None` for any value type that does not override it (including entries
    /// that are expired), and it may be in the past for an entry
    /// [`Expires::is_expired`] reports as live. `cache_peek_with_expiry_status` (on
    /// [`ConcurrentCloneCached`]), which consults `is_expired`, remains the authoritative
    /// liveness read here; see the [`ConcurrentCacheExpiry`] trait docs for the caveat in full.
    /// An expired entry is returned and is **not** removed; the hits/misses counters are
    /// untouched.
    fn cache_peek_expires_at(&self, k: &K) -> (Option<V>, Option<crate::time::Instant>) {
        let shard = self.shard_of(k);
        let guard = shard.lock.read();
        match guard.get(k) {
            None => (None, None),
            Some(v) => (Some(v.clone()), v.expires_at()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ConcurrentCached;
    use crate::ConcurrentCached as SyncConcurrentCached;
    use crate::ConcurrentCachedExt as SyncConcurrentCachedExt;
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

    /// A value type that overrides `expires_at` to report a concrete deadline, unlike `Val`
    /// which relies on the trait default. `is_expired` is set independently of `deadline` so
    /// tests can construct either a live or an expired entry with a known deadline.
    #[derive(Clone)]
    struct TimedVal {
        v: u32,
        expired: bool,
        deadline: crate::time::Instant,
    }
    impl crate::Expires for TimedVal {
        fn is_expired(&self) -> bool {
            self.expired
        }
        fn expires_at(&self) -> Option<crate::time::Instant> {
            Some(self.deadline)
        }
    }

    #[test]
    fn cache_set_over_expired_returns_none_fires_on_evict_and_counts() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU64, Ordering as AOrd};
        let count = Arc::new(AtomicU64::new(0));
        let count2 = count.clone();
        let c = ShardedExpiringCache::<u32, Val>::builder()
            .on_evict(move |_, _| {
                count2.fetch_add(1, AOrd::Relaxed);
            })
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(
            &c,
            1,
            Val {
                v: 1,
                expired: true,
            },
        )
        .unwrap();
        let before = c.metrics().evictions.unwrap();
        // Overwriting an expired value: None returned, on_evict fires once, one eviction.
        assert_eq!(
            SyncConcurrentCached::cache_set(
                &c,
                1,
                Val {
                    v: 2,
                    expired: false
                }
            )
            .unwrap()
            .map(|v| v.v),
            None
        );
        assert_eq!(c.metrics().evictions.unwrap(), before + 1);
        assert_eq!(count.load(AOrd::Relaxed), 1);
        // Overwriting a live value returns it, no on_evict and no new eviction.
        assert_eq!(
            SyncConcurrentCached::cache_set(
                &c,
                1,
                Val {
                    v: 3,
                    expired: false
                }
            )
            .unwrap()
            .map(|v| v.v),
            Some(2)
        );
        assert_eq!(c.metrics().evictions.unwrap(), before + 1);
        assert_eq!(count.load(AOrd::Relaxed), 1);
    }

    #[test]
    fn cache_set_over_expired_counts_eviction_without_callback() {
        // Pins that the evictions counter increments when overwriting an expired entry
        // even when no on_evict callback is configured.
        let c = ShardedExpiringCache::<u32, Val>::builder().build().unwrap();
        SyncConcurrentCached::cache_set(
            &c,
            1,
            Val {
                v: 1,
                expired: true,
            },
        )
        .unwrap();
        let before = c.metrics().evictions.unwrap();
        assert_eq!(
            SyncConcurrentCached::cache_set(
                &c,
                1,
                Val {
                    v: 2,
                    expired: false
                }
            )
            .unwrap()
            .map(|v| v.v),
            None
        );
        assert_eq!(
            c.metrics().evictions.unwrap(),
            before + 1,
            "evictions must increment by 1 on expired-entry overwrite even without on_evict"
        );
    }

    #[test]
    fn new_returns_ready_cache() {
        let c = ShardedExpiringCache::<u32, Val>::new();
        assert_eq!(
            SyncConcurrentCachedExt::set(
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
            SyncConcurrentCachedExt::get(&c, &1).unwrap().map(|v| v.v),
            Some(10)
        );
        // Expired values are not returned.
        SyncConcurrentCachedExt::set(
            &c,
            2,
            Val {
                v: 20,
                expired: true,
            },
        )
        .unwrap();
        assert!(SyncConcurrentCachedExt::get(&c, &2).unwrap().is_none());
    }

    #[test]
    fn retain_fires_on_evict_after_the_shard_lock_is_released() {
        // The callback must observe every shard lock as free: `retain` collects the
        // removed pairs under the shard write guard, drops it, and only then fires
        // `on_evict`. `try_write` returning `None` for any shard would mean the guard
        // was still held while the callback ran.
        use std::sync::OnceLock;
        use std::sync::atomic::{AtomicU64, Ordering as AOrd};

        let handle: Arc<OnceLock<ShardedExpiringCache<u32, Val>>> = Arc::new(OnceLock::new());
        let handle2 = handle.clone();
        let fired = Arc::new(AtomicU64::new(0));
        let fired2 = fired.clone();
        let c = ShardedExpiringCache::<u32, Val>::builder()
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
                fired2.fetch_add(1, AOrd::Relaxed);
            })
            .build()
            .unwrap();
        handle.set(c.clone()).expect("handle set once");
        for i in 0..32u32 {
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
        c.retain(|_k, _v| false);
        assert_eq!(c.len(), 0, "a keep-nothing predicate empties every shard");
        assert_eq!(
            fired.load(AOrd::Relaxed),
            32,
            "on_evict fires exactly once per removed entry"
        );
    }

    #[test]
    fn retain_sweeps_every_shard_one_at_a_time() {
        // Per-shard bookkeeping: the predicate is applied to each shard's own map, so the
        // post-retain per-shard counts equal the surviving keys routed to that shard.
        let c = ShardedExpiringCache::<u32, Val>::builder()
            .shards(4)
            .build()
            .unwrap();
        for i in 0..64u32 {
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
        let new_cache = ShardedExpiringCache::<u32, Val>::builder()
            .copy_from(&old)
            .unwrap();
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
        let new_cache = ShardedExpiringCache::<u32, Val>::builder()
            .copy_from(&old)
            .unwrap();
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
        let cache = ShardedExpiringCache::<u32, Val>::builder()
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
        let result = ShardedExpiringCache::<u32, Val>::builder()
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
        let c = ShardedExpiringCache::<u32, Val>::builder()
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
        let c = ShardedExpiringCache::<u32, Val>::builder()
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
    fn cache_clear_with_on_evict_counts_evictions_without_callback() {
        // metrics().evictions must not depend on an on_evict observer being attached.
        let c = ShardedExpiringCache::<u32, Val>::builder().build().unwrap();
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
        let before = c.metrics().evictions.expect("evictions tracked");
        c.cache_clear_with_on_evict();
        assert_eq!(c.len(), 0);
        assert_eq!(
            c.metrics().evictions.expect("evictions tracked") - before,
            20,
            "evictions must be counted even with no on_evict callback"
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
        let c = ShardedExpiringCache::<u32, Val>::builder()
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
        let c = ShardedExpiringCache::<u32, Val>::builder()
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
        let c = ShardedExpiringCache::<u32, Val>::builder()
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
        let c = ShardedExpiringCache::<u32, Val>::builder()
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
        let c = ShardedExpiringCache::<u32, Val>::builder()
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

    // --- ConcurrentCacheExpiry tests ---

    #[test]
    fn peek_expires_at_absent_key_returns_none_none() {
        let c = ShardedExpiringCache::<u32, Val>::builder().build().unwrap();
        let (value, expires_at) = ConcurrentCacheExpiry::cache_peek_expires_at(&c, &1);
        assert!(value.is_none());
        assert_eq!(expires_at, None);
        let (value, expires_at) = ConcurrentCacheExpiry::peek_expires_at(&c, &1);
        assert!(value.is_none());
        assert_eq!(expires_at, None);
    }

    #[test]
    fn peek_expires_at_live_entry_returns_the_stored_future_deadline() {
        let c = ShardedExpiringCache::<u32, TimedVal>::builder()
            .build()
            .unwrap();
        let deadline = crate::time::Instant::now() + std::time::Duration::from_secs(3600);
        SyncConcurrentCached::cache_set(
            &c,
            1u32,
            TimedVal {
                v: 10,
                expired: false,
                deadline,
            },
        )
        .expect("insert must succeed");

        let (value, expires_at) = ConcurrentCacheExpiry::cache_peek_expires_at(&c, &1u32);
        assert_eq!(value.map(|x| x.v), Some(10));
        assert_eq!(
            expires_at,
            Some(deadline),
            "the reported deadline must be the one the value reports"
        );
    }

    #[test]
    fn peek_expires_at_never_expiring_entry_reports_no_deadline() {
        let c = ShardedExpiringCache::<u32, Val>::builder().build().unwrap();
        SyncConcurrentCached::cache_set(
            &c,
            1u32,
            Val {
                v: 10,
                expired: false,
            },
        )
        .expect("insert must succeed");
        let (value, expires_at) = ConcurrentCacheExpiry::cache_peek_expires_at(&c, &1u32);
        assert_eq!(value.map(|x| x.v), Some(10));
        assert_eq!(
            expires_at, None,
            "a value type that does not override expires_at reports no deadline"
        );
        // Distinguishable from an absent key by the value, not by the deadline.
        let (val, expired) = ConcurrentCloneCached::cache_peek_with_expiry_status(&c, &1u32);
        assert_eq!(val.map(|x| x.v), Some(10));
        assert!(!expired);
    }

    #[test]
    fn peek_expires_at_expired_entry_returns_a_past_deadline_and_keeps_the_entry() {
        let c = ShardedExpiringCache::<u32, TimedVal>::builder()
            .shards(1)
            .build()
            .unwrap();
        let deadline = crate::time::Instant::now() - std::time::Duration::from_secs(3600);
        SyncConcurrentCached::cache_set(
            &c,
            1u32,
            TimedVal {
                v: 10,
                expired: true,
                deadline,
            },
        )
        .expect("insert must succeed");

        let before = c.metrics();
        let (value, expires_at) = ConcurrentCacheExpiry::cache_peek_expires_at(&c, &1u32);
        assert_eq!(
            value.map(|x| x.v),
            Some(10),
            "an expired entry is still returned"
        );
        assert_eq!(expires_at, Some(deadline), "the past deadline is reported");

        // Not removed by the peek, and no eviction counted.
        let after = c.metrics();
        assert_eq!(after.evictions, before.evictions);
        let (value2, expires_at2) = ConcurrentCloneCached::cache_peek_with_expiry_status(&c, &1u32);
        assert_eq!(value2.map(|x| x.v), Some(10));
        assert!(
            expires_at2,
            "the entry must still report expired via is_expired"
        );
    }

    #[test]
    fn peek_expires_at_does_not_touch_hit_or_miss_counters() {
        let c = ShardedExpiringCache::<u32, Val>::builder().build().unwrap();
        SyncConcurrentCached::cache_set(
            &c,
            1u32,
            Val {
                v: 10,
                expired: false,
            },
        )
        .expect("insert must succeed");
        let hits = c.metrics().hits;
        let misses = c.metrics().misses;

        let _ = ConcurrentCacheExpiry::cache_peek_expires_at(&c, &1u32); // present
        let _ = ConcurrentCacheExpiry::cache_peek_expires_at(&c, &2u32); // absent

        assert_eq!(c.metrics().hits, hits, "a peek must not count a hit");
        assert_eq!(c.metrics().misses, misses, "a peek must not count a miss");
    }

    #[test]
    fn peek_expires_at_expired_value_without_expires_at_override_reports_no_deadline() {
        // Pins the documented Expires-store caveat: `None` does not mean live. `Val` does not
        // override `expires_at`, so an expired `Val` still reports `None`, not a past instant.
        let c = ShardedExpiringCache::<u32, Val>::builder().build().unwrap();
        SyncConcurrentCached::cache_set(
            &c,
            1u32,
            Val {
                v: 10,
                expired: true,
            },
        )
        .expect("insert must succeed");

        let (value, expires_at) = ConcurrentCacheExpiry::cache_peek_expires_at(&c, &1u32);
        assert_eq!(
            value.map(|x| x.v),
            Some(10),
            "an expired entry is still returned"
        );
        assert_eq!(
            expires_at, None,
            "None does not mean live: this value type never reported a deadline"
        );
        // is_expired remains the authoritative liveness read.
        let (_, expired) = ConcurrentCloneCached::cache_peek_with_expiry_status(&c, &1u32);
        assert!(
            expired,
            "is_expired is authoritative, unlike the advisory expires_at"
        );
    }

    #[test]
    fn peek_expires_at_multi_shard() {
        let c = ShardedExpiringCache::<u32, Val>::builder()
            .shards(8)
            .build()
            .unwrap();
        assert_eq!(c.shards(), 8);

        for i in 0..32u32 {
            SyncConcurrentCached::cache_set(
                &c,
                i,
                Val {
                    v: i * 10,
                    expired: false,
                },
            )
            .expect("insert must succeed");
        }
        for i in 0..32u32 {
            let (value, expires_at) = ConcurrentCacheExpiry::cache_peek_expires_at(&c, &i);
            assert_eq!(
                value.map(|x| x.v),
                Some(i * 10),
                "key {i} must route to the right shard"
            );
            assert_eq!(expires_at, None);
        }
        let (value, expires_at) = ConcurrentCacheExpiry::cache_peek_expires_at(&c, &999u32);
        assert!(value.is_none());
        assert_eq!(expires_at, None);
    }

    // Certification gap (Q1): the existing "expired entry keeps a past deadline" test
    // (`peek_expires_at_expired_entry_returns_a_past_deadline_and_keeps_the_entry`) sets
    // `is_expired` and `deadline` CONSISTENTLY. The documented caveat (specs/design/0047, TRAIT-6
    // / CTRAIT-9) is bidirectional: the advisory deadline "can be in the past for an entry
    // is_expired() still reports live" -- the OTHER direction, not yet pinned anywhere in this
    // file. Mirrors the single-owner `ExpiringCache`'s
    // `peek_expires_at_advisory_past_deadline_survives_while_is_expired_reports_live`.
    #[test]
    fn peek_expires_at_advisory_past_deadline_survives_while_is_expired_reports_live() {
        let past = crate::time::Instant::now() - std::time::Duration::from_secs(3600);
        let c = ShardedExpiringCache::<u32, TimedVal>::builder()
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(
            &c,
            1u32,
            TimedVal {
                v: 10,
                expired: false, // is_expired() reports LIVE
                deadline: past, // yet the advisory deadline is already in the past
            },
        )
        .expect("insert must succeed");

        let (value, expires_at) = ConcurrentCacheExpiry::cache_peek_expires_at(&c, &1u32);
        assert_eq!(value.map(|x| x.v), Some(10));
        assert_eq!(
            expires_at,
            Some(past),
            "the advisory deadline must be surfaced unchanged, even though it is in the past"
        );
        let (val, expired) = ConcurrentCloneCached::cache_peek_with_expiry_status(&c, &1u32);
        assert_eq!(val.map(|x| x.v), Some(10));
        assert!(
            !expired,
            "cache_peek_with_expiry_status must still report the entry as live"
        );
    }

    #[test]
    fn peek_expires_at_reports_absent_after_evict_removes_the_entry() {
        let c = ShardedExpiringCache::<u32, Val>::builder().build().unwrap();
        SyncConcurrentCached::cache_set(
            &c,
            1u32,
            Val {
                v: 100,
                expired: true,
            },
        )
        .expect("insert must succeed");

        let (value, expires_at) = ConcurrentCacheExpiry::cache_peek_expires_at(&c, &1u32);
        assert_eq!(value.map(|x| x.v), Some(100));
        assert_eq!(expires_at, None, "Val never overrides expires_at");

        assert_eq!(
            c.evict(),
            1,
            "evict must physically remove the expired entry"
        );
        let (value, expires_at) = ConcurrentCacheExpiry::cache_peek_expires_at(&c, &1u32);
        assert!(
            value.is_none(),
            "a physically removed entry must be reported as absent"
        );
        assert_eq!(expires_at, None);
    }

    #[test]
    fn peek_expires_at_reports_absent_after_cache_remove() {
        let c = ShardedExpiringCache::<u32, Val>::builder().build().unwrap();
        SyncConcurrentCached::cache_set(
            &c,
            1u32,
            Val {
                v: 100,
                expired: false,
            },
        )
        .expect("insert must succeed");
        let removed = SyncConcurrentCached::cache_remove(&c, &1u32).unwrap();
        assert_eq!(removed.map(|v| v.v), Some(100));

        let (value, expires_at) = ConcurrentCacheExpiry::cache_peek_expires_at(&c, &1u32);
        assert!(value.is_none());
        assert_eq!(expires_at, None);
        let (value, expires_at) = ConcurrentCacheExpiry::peek_expires_at(&c, &1u32);
        assert!(value.is_none());
        assert_eq!(expires_at, None);
    }

    // Gap: the ergonomic alias must agree with the canonical method across every return shape
    // the contract defines, not just the absent-key case the implementor already covered.
    #[test]
    fn peek_expires_at_alias_matches_canonical_across_all_return_shapes() {
        fn shape(
            pair: (Option<TimedVal>, Option<crate::time::Instant>),
        ) -> (Option<u32>, Option<crate::time::Instant>) {
            (pair.0.map(|v| v.v), pair.1)
        }

        let c = ShardedExpiringCache::<u32, TimedVal>::builder()
            .build()
            .unwrap();

        // absent
        assert_eq!(
            shape(ConcurrentCacheExpiry::peek_expires_at(&c, &1)),
            shape(ConcurrentCacheExpiry::cache_peek_expires_at(&c, &1))
        );
        assert_eq!(
            shape(ConcurrentCacheExpiry::peek_expires_at(&c, &1)),
            (None, None)
        );

        // live, future deadline
        let future = crate::time::Instant::now() + std::time::Duration::from_secs(3600);
        SyncConcurrentCached::cache_set(
            &c,
            1,
            TimedVal {
                v: 100,
                expired: false,
                deadline: future,
            },
        )
        .expect("insert must succeed");
        assert_eq!(
            shape(ConcurrentCacheExpiry::peek_expires_at(&c, &1)),
            shape(ConcurrentCacheExpiry::cache_peek_expires_at(&c, &1))
        );
        assert_eq!(
            shape(ConcurrentCacheExpiry::peek_expires_at(&c, &1)),
            (Some(100), Some(future))
        );

        // expired, not removed, past deadline
        let past = crate::time::Instant::now() - std::time::Duration::from_secs(3600);
        SyncConcurrentCached::cache_set(
            &c,
            2,
            TimedVal {
                v: 200,
                expired: true,
                deadline: past,
            },
        )
        .expect("insert must succeed");
        assert_eq!(
            shape(ConcurrentCacheExpiry::peek_expires_at(&c, &2)),
            shape(ConcurrentCacheExpiry::cache_peek_expires_at(&c, &2))
        );
        assert_eq!(
            shape(ConcurrentCacheExpiry::peek_expires_at(&c, &2)),
            (Some(200), Some(past))
        );

        // a value type never overriding expires_at
        let c2 = ShardedExpiringCache::<u32, Val>::builder().build().unwrap();
        SyncConcurrentCached::cache_set(
            &c2,
            1,
            Val {
                v: 5,
                expired: false,
            },
        )
        .expect("insert must succeed");
        let (v1, e1) = ConcurrentCacheExpiry::peek_expires_at(&c2, &1);
        let (v2, e2) = ConcurrentCacheExpiry::cache_peek_expires_at(&c2, &1);
        assert_eq!(v1.as_ref().map(|v| v.v), v2.map(|v| v.v));
        assert_eq!(e1, e2);
        assert_eq!((v1.map(|v| v.v), e1), (Some(5), None));
    }

    // Gap: nothing else in the suite calls `ConcurrentCacheExpiry` through a generic
    // `T: ConcurrentCacheExpiry<K, V>` bound or through `cached::prelude::*` -- both a
    // monomorphization/dyn-compat regression and a prelude export regression would go
    // uncaught otherwise.
    #[test]
    fn concurrent_cache_expiry_is_reachable_through_a_generic_bound_and_the_prelude() {
        use crate::prelude::*;

        fn peek_via_bound<T: ConcurrentCacheExpiry<u32, Val>>(
            store: &T,
            key: &u32,
        ) -> (Option<Val>, Option<crate::time::Instant>) {
            store.cache_peek_expires_at(key)
        }

        let c = ShardedExpiringCache::<u32, Val>::builder().build().unwrap();
        SyncConcurrentCached::cache_set(
            &c,
            1,
            Val {
                v: 100,
                expired: false,
            },
        )
        .expect("insert must succeed");

        let (value, expires_at) = peek_via_bound(&c, &1);
        assert_eq!(value.map(|v| v.v), Some(100));
        assert_eq!(expires_at, None);
        let (value, expires_at) = peek_via_bound(&c, &2);
        assert!(value.is_none());
        assert_eq!(expires_at, None, "absent key via the generic bound");
    }

    // Certification gap (Q3): existing `peek_expires_at` tests only ever route a single
    // uniform group of keys. Route a MIX of expired and live keys through a multi-shard cache
    // and confirm each is read back correctly, regardless of which physical shard it landed in.
    #[test]
    fn peek_expires_at_routes_correctly_across_many_shards() {
        let c = ShardedExpiringCache::<u32, Val>::builder()
            .shards(8)
            .build()
            .unwrap();
        assert_eq!(c.shards(), 8);

        for i in 0..24u32 {
            SyncConcurrentCached::cache_set(
                &c,
                i,
                Val {
                    v: i * 10,
                    expired: i < 8, // keys 0..8 expired, 8..24 live
                },
            )
            .expect("insert must succeed");
        }

        // Sanity: the fixture must actually span multiple physical shards, otherwise this test
        // would not exercise cross-shard routing at all.
        let distinct_shards: std::collections::HashSet<usize> = (0..24u32)
            .map(|k| c.shard_of(&k) as *const _ as usize)
            .collect();
        assert!(
            distinct_shards.len() > 1,
            "fixture must span multiple shards for this test to be meaningful"
        );

        for i in 0..24u32 {
            let (value, expires_at) = ConcurrentCacheExpiry::cache_peek_expires_at(&c, &i);
            assert_eq!(value.map(|x| x.v), Some(i * 10), "key {i}");
            assert_eq!(expires_at, None, "Val never overrides expires_at");
            let (_, expired) = ConcurrentCloneCached::cache_peek_with_expiry_status(&c, &i);
            assert_eq!(expired, i < 8, "key {i} expiry status must match its group");
        }
        let (value, expires_at) = ConcurrentCacheExpiry::cache_peek_expires_at(&c, &999u32);
        assert!(value.is_none());
        assert_eq!(expires_at, None);
    }

    // Certification gap (read-lock-only property): a writer on one thread (through an
    // Arc-shared clone) overwrites a key with a fresh deadline; a subsequent peek on the
    // original handle, after the writer has joined, must observe that update -- exercising
    // cross-thread visibility through the shard lock rather than the single-threaded
    // round-trips every other `peek_expires_at` test performs.
    #[test]
    fn peek_expires_at_observes_a_concurrent_writers_deadline_update() {
        let c = ShardedExpiringCache::<u32, TimedVal>::builder()
            .shards(8)
            .build()
            .unwrap();
        let before_deadline = crate::time::Instant::now() + std::time::Duration::from_secs(60);
        SyncConcurrentCached::cache_set(
            &c,
            1,
            TimedVal {
                v: 10,
                expired: false,
                deadline: before_deadline,
            },
        )
        .expect("insert must succeed");

        // Arc-share clone: both handles refer to the same underlying shards.
        let writer = c.clone();
        let after_deadline = before_deadline + std::time::Duration::from_secs(3600);
        let handle = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(30));
            SyncConcurrentCached::cache_set(
                &writer,
                1,
                TimedVal {
                    v: 20,
                    expired: false,
                    deadline: after_deadline,
                },
            )
            .expect("overwrite must succeed");
        });
        handle.join().expect("writer thread must not panic");

        let (value, expires_at) = ConcurrentCacheExpiry::cache_peek_expires_at(&c, &1);
        assert_eq!(
            value.map(|v| v.v),
            Some(20),
            "peek on the original handle must observe the writer thread's value"
        );
        assert_eq!(
            expires_at,
            Some(after_deadline),
            "peek must observe the writer thread's updated deadline"
        );
    }

    // --- Inherent infallible method tests ---

    #[test]
    fn inherent_get_returns_option_not_result() {
        let c = ShardedExpiringCache::<u32, Val>::new();
        let v: Option<Val> = c.get(&1);
        assert!(v.is_none());
        c.set(
            1,
            Val {
                v: 42,
                expired: false,
            },
        );
        let v: Option<Val> = c.get(&1);
        assert_eq!(v.map(|x| x.v), Some(42));
    }

    #[test]
    fn inherent_get_returns_none_for_expired() {
        let c = ShardedExpiringCache::<u32, Val>::new();
        c.set(
            1,
            Val {
                v: 99,
                expired: true,
            },
        );
        // Expired entries are filtered out by get.
        let v: Option<Val> = c.get(&1);
        assert!(
            v.is_none(),
            "expired entry must return None from inherent get"
        );
    }

    #[test]
    fn inherent_set_returns_previous_value() {
        let c = ShardedExpiringCache::<u32, Val>::new();
        let prev: Option<Val> = c.set(
            1,
            Val {
                v: 10,
                expired: false,
            },
        );
        assert!(prev.is_none());
        let prev: Option<Val> = c.set(
            1,
            Val {
                v: 20,
                expired: false,
            },
        );
        assert_eq!(prev.map(|x| x.v), Some(10));
        assert_eq!(c.get(&1).map(|x| x.v), Some(20));
    }

    #[test]
    fn inherent_remove_returns_prior_live_value() {
        let c = ShardedExpiringCache::<u32, Val>::new();
        c.set(
            1,
            Val {
                v: 99,
                expired: false,
            },
        );
        let v: Option<Val> = c.remove(&1);
        assert_eq!(v.map(|x| x.v), Some(99));
        assert!(c.remove(&1).is_none());
    }

    #[test]
    fn inherent_remove_entry_returns_key_and_value() {
        let c = ShardedExpiringCache::<u32, Val>::new();
        c.set(
            7,
            Val {
                v: 77,
                expired: false,
            },
        );
        let pair: Option<(u32, Val)> = c.remove_entry(&7);
        assert_eq!(pair.map(|(k, v)| (k, v.v)), Some((7, 77)));
        assert!(c.remove_entry(&7).is_none());
    }

    #[test]
    fn inherent_delete_returns_bool() {
        let c = ShardedExpiringCache::<u32, Val>::new();
        c.set(
            1,
            Val {
                v: 10,
                expired: false,
            },
        );
        let removed: bool = c.delete(&1);
        assert!(removed);
        let removed: bool = c.delete(&1);
        assert!(!removed);
    }

    // B4 regression: deep_clone must load hit/miss counters under the read lock so the
    // metrics snapshot is consistent with the captured entry state.  After performing
    // a fixed number of gets, the cloned cache's metrics must reflect exactly those
    // operations (not a potentially newer reading from after the lock was released).
    #[test]
    fn deep_clone_metrics_consistent_with_entry_snapshot() {
        let c = ShardedExpiringCache::<u32, Val>::builder()
            .shards(1) // single shard: deterministic counters
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(
            &c,
            1,
            Val {
                v: 1,
                expired: false,
            },
        )
        .unwrap();
        // Generate exactly 2 hits and 1 miss.
        SyncConcurrentCached::cache_get(&c, &1).unwrap(); // hit
        SyncConcurrentCached::cache_get(&c, &1).unwrap(); // hit
        SyncConcurrentCached::cache_get(&c, &99).unwrap(); // miss

        let clone = c.deep_clone();
        let m = clone.metrics();
        assert_eq!(m.hits, Some(2), "deep_clone must capture the hit counter");
        assert_eq!(
            m.misses,
            Some(1),
            "deep_clone must capture the miss counter"
        );
        assert_eq!(clone.len(), 1, "deep_clone must capture the entry snapshot");
    }

    // B2 regression: is_expired() is evaluated while the write lock is held, so the
    // decision to fire on_evict and return None is consistent with the state observed
    // under the lock, not a later (possibly different) instant.
    #[test]
    fn displaced_expired_entry_skips_return_fires_on_evict_and_counts() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU64, Ordering as AOrd};
        let fired = Arc::new(AtomicU64::new(0));
        let fired2 = fired.clone();
        let c = ShardedExpiringCache::<u32, Val>::builder()
            .on_evict(move |_, _| {
                fired2.fetch_add(1, AOrd::Relaxed);
            })
            .build()
            .unwrap();
        // Insert a value that is already expired.
        SyncConcurrentCached::cache_set(
            &c,
            42,
            Val {
                v: 1,
                expired: true,
            },
        )
        .unwrap();
        let before_evictions = c.metrics().evictions.unwrap();
        // Overwriting the expired entry: must return None (not the expired value),
        // must fire on_evict exactly once, and must count one eviction.
        let result = SyncConcurrentCached::cache_set(
            &c,
            42,
            Val {
                v: 2,
                expired: false,
            },
        )
        .unwrap()
        .map(|v| v.v);
        assert_eq!(result, None, "displaced expired entry must not be returned");
        assert_eq!(
            c.metrics().evictions.unwrap(),
            before_evictions + 1,
            "eviction counter must increment for displaced expired entry"
        );
        assert_eq!(
            fired.load(AOrd::Relaxed),
            1,
            "on_evict must fire exactly once for the displaced expired entry"
        );
        // Overwriting a live entry returns the old value and does not fire on_evict.
        let before_evictions2 = c.metrics().evictions.unwrap();
        let result2 = SyncConcurrentCached::cache_set(
            &c,
            42,
            Val {
                v: 3,
                expired: false,
            },
        )
        .unwrap()
        .map(|v| v.v);
        assert_eq!(
            result2,
            Some(2),
            "displaced live entry must be returned as Some"
        );
        assert_eq!(
            c.metrics().evictions.unwrap(),
            before_evictions2,
            "overwriting a live entry must not increment evictions"
        );
        assert_eq!(
            fired.load(AOrd::Relaxed),
            1,
            "on_evict must not fire again for a displaced live entry"
        );
    }

    // --- Per-shard evictions counter aggregation (internal-only refactor coverage) ---

    #[test]
    fn evictions_aggregate_across_multiple_shards_via_metrics_and_cache_evictions() {
        // Force many shards so evictions land on distinct per-shard counters, then
        // confirm both metrics().evictions and the trait-level cache_evictions()
        // sum every shard exactly (no double counting, no dropped counts).
        let c = ShardedExpiringCache::<u32, Val>::builder()
            .shards(8)
            .build()
            .unwrap();
        for i in 0..64u32 {
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
        // Remove every entry — cache_remove increments the per-shard evictions
        // counter for whichever shard the key hashes into.
        for i in 0..64u32 {
            SyncConcurrentCached::cache_remove(&c, &i).expect("remove must succeed");
        }
        assert_eq!(
            c.metrics().evictions,
            Some(64),
            "metrics().evictions must sum every shard's counter"
        );
        assert_eq!(
            ConcurrentCacheBase::cache_evictions(&c),
            Some(64),
            "cache_evictions() must sum every shard's counter"
        );
        // Sanity: with 8 shards and 64 distinct keys, more than one shard must have
        // actually recorded an eviction (otherwise this test would pass trivially
        // even if only shard 0's counter were summed).
        let nonzero_shards = c
            .inner
            .shards
            .iter()
            .filter(|s| s.evictions.load(Ordering::Relaxed) > 0)
            .count();
        assert!(
            nonzero_shards > 1,
            "evictions must be spread across multiple shards for this to be a meaningful test"
        );
    }

    #[test]
    fn evictions_aggregate_correctly_after_deep_clone() {
        // deep_clone must carry each shard's evictions counter into the corresponding
        // cloned shard so the aggregate reported by the clone matches the source.
        let c = ShardedExpiringCache::<u32, Val>::builder()
            .shards(8)
            .build()
            .unwrap();
        for i in 0..64u32 {
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
        // Evict half of them (odd keys) so several shards record nonzero evictions.
        for i in (0..64u32).step_by(2) {
            SyncConcurrentCached::cache_remove(&c, &i).expect("remove must succeed");
        }
        let before = c.metrics().evictions.unwrap();
        assert_eq!(before, 32);

        let clone = c.deep_clone();
        assert_eq!(
            clone.metrics().evictions,
            Some(32),
            "deep_clone must carry the summed evictions count through unchanged"
        );
        // Per-shard carry-over: every shard's evictions counter in the clone must match
        // the corresponding source shard, not just the aggregate.
        for (src, cloned) in c.inner.shards.iter().zip(clone.inner.shards.iter()) {
            assert_eq!(
                src.evictions.load(Ordering::Relaxed),
                cloned.evictions.load(Ordering::Relaxed),
                "deep_clone must carry each shard's evictions counter individually"
            );
        }

        // The clone and the source must be independent from here on.
        SyncConcurrentCached::cache_remove(&clone, &1u32).expect("remove must succeed");
        assert_eq!(
            clone.metrics().evictions,
            Some(33),
            "post-clone evictions on the clone must not affect the source"
        );
        assert_eq!(
            c.metrics().evictions,
            Some(32),
            "post-clone evictions on the clone must not leak back to the source"
        );
    }

    // --- One-pass evict() coverage ---

    #[test]
    fn evict_with_callback_fires_exactly_once_per_removed_entry_with_correct_pairs() {
        use std::sync::Mutex;

        let seen: Arc<Mutex<Vec<(u32, u32)>>> = Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        let c = ShardedExpiringCache::<u32, Val>::builder()
            .shards(4)
            .on_evict(move |k, v| {
                seen2.lock().unwrap().push((*k, v.v));
            })
            .build()
            .unwrap();

        // Live entries that must survive the sweep.
        for i in 0..10u32 {
            SyncConcurrentCached::cache_set(
                &c,
                i,
                Val {
                    v: i * 100,
                    expired: false,
                },
            )
            .expect("insert must succeed");
        }
        // Expired entries that must be swept, with distinguishable values.
        for i in 10..20u32 {
            SyncConcurrentCached::cache_set(
                &c,
                i,
                Val {
                    v: i * 100,
                    expired: true,
                },
            )
            .expect("insert must succeed");
        }

        let removed_count = c.evict();
        assert_eq!(removed_count, 10, "evict must report exactly 10 removed");
        assert_eq!(
            c.len(),
            10,
            "only the 10 expired entries must have been removed"
        );

        let mut got = seen.lock().unwrap().clone();
        got.sort_unstable();
        let mut expected: Vec<(u32, u32)> = (10..20u32).map(|i| (i, i * 100)).collect();
        expected.sort_unstable();
        assert_eq!(
            got, expected,
            "on_evict must fire exactly once per removed entry with the correct (k, v) pair"
        );

        // Live entries must all still be retrievable.
        for i in 0..10u32 {
            assert_eq!(
                SyncConcurrentCached::cache_get(&c, &i)
                    .expect("cache_get must succeed")
                    .map(|v| v.v),
                Some(i * 100),
                "live entry {i} must survive evict()"
            );
        }

        // A second evict() call finds nothing left to remove.
        assert_eq!(c.evict(), 0, "a second evict() call must be a no-op");
        assert_eq!(seen.lock().unwrap().len(), 10, "no further callbacks fire");
    }

    #[test]
    fn evict_without_callback_returns_correct_count_and_removes_entries() {
        // No on_evict configured: the single-pass retain+len-delta branch must still
        // return the correct count, physically remove the expired entries, and
        // increment the per-shard evictions counters aggregated via metrics().
        let c = ShardedExpiringCache::<u32, Val>::builder()
            .shards(4)
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
        for i in 10..25u32 {
            SyncConcurrentCached::cache_set(
                &c,
                i,
                Val {
                    v: i,
                    expired: true,
                },
            )
            .expect("insert must succeed");
        }
        assert_eq!(c.len(), 25);

        let before_evictions = c.metrics().evictions.unwrap();
        let removed_count = c.evict();
        assert_eq!(
            removed_count, 15,
            "evict must report exactly the 15 expired entries removed"
        );
        assert_eq!(
            c.len(),
            10,
            "expired entries must be physically removed from the map"
        );
        assert_eq!(
            c.metrics().evictions.unwrap() - before_evictions,
            15,
            "evictions must be counted through the no-callback branch too"
        );

        for i in 0..10u32 {
            assert!(
                SyncConcurrentCached::cache_get(&c, &i)
                    .expect("cache_get must succeed")
                    .is_some(),
                "live entry {i} must survive evict() with no callback"
            );
        }

        assert_eq!(
            c.evict(),
            0,
            "a second evict() call with no callback must be a no-op"
        );
    }

    // --- cache_clear_with_on_evict early-return (no-callback) path ---

    #[test]
    fn cache_clear_with_on_evict_no_callback_counts_via_early_return_across_shards() {
        // Exercises the item-3 early-return branch across multiple shards, confirming
        // it still counts every removed entry as an eviction (via the per-shard
        // counters summed in metrics()/cache_evictions()) without ever building a
        // Vec of the removed entries.
        let c = ShardedExpiringCache::<u32, Val>::builder()
            .shards(8)
            .build()
            .unwrap();
        for i in 0..40u32 {
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
        let before = c.metrics().evictions.unwrap();
        c.cache_clear_with_on_evict();
        assert_eq!(
            c.len(),
            0,
            "cache must be empty after the early-return path"
        );
        assert_eq!(
            c.metrics().evictions.unwrap() - before,
            40,
            "every removed entry must be counted even via the no-callback early return"
        );
        assert_eq!(
            ConcurrentCacheBase::cache_evictions(&c),
            Some(40),
            "cache_evictions() must reflect the early-return path's counts too"
        );
    }

    #[test]
    fn inherent_and_trait_methods_coexist_via_fully_qualified_path() {
        fn use_trait<C>(cache: &C, k: u32, v: Val)
        where
            C: SyncConcurrentCached<u32, Val>,
        {
            let _: Result<Option<Val>, _> = ConcurrentCached::cache_set(cache, k, v);
            let _: Result<Option<Val>, _> = ConcurrentCached::cache_get(cache, &k);
            let _: Result<Option<Val>, _> = ConcurrentCached::cache_remove(cache, &k);
        }
        let c = ShardedExpiringCache::<u32, Val>::new();
        use_trait(
            &c,
            1,
            Val {
                v: 42,
                expired: false,
            },
        );
    }

    // --- Certification: adversarial coverage of the per-shard eviction counter rewrite ---
    //
    // The rewrite removed `ExpiringInner.evictions: AtomicU64` in favor of one counter per
    // shard, and turned `evict()` into a single-pass sweep. These tests approach that from
    // the outside: per-shard placement (not just the aggregate), the two `evict` branches
    // agreeing exactly, `deep_clone` after a mix of eviction paths, and concurrent
    // evict()/cache_remove()/cache_set() hitting many shards at once without losing or
    // double-counting an eviction.

    /// Raw per-shard eviction counters, in shard order.
    fn shard_eviction_counters<K, V, H>(c: &ShardedExpiringCache<K, V, H>) -> Vec<u64> {
        c.inner
            .shards
            .iter()
            .map(|s| s.evictions.load(Ordering::Relaxed))
            .collect()
    }

    /// Index of the shard that owns `k`.
    fn owning_shard<K, V, H: ShardHasher<K>>(c: &ShardedExpiringCache<K, V, H>, k: &K) -> usize {
        shard_index(c.inner.hasher.shard_hash(k), c.inner.shard_mask)
    }

    /// Deterministic shard placement for cross-cache comparisons. `DefaultShardHasher` is
    /// randomly seeded per instance, so two independently built caches would scatter the
    /// same keys onto different shards and a shard-for-shard comparison between them would
    /// be meaningless.
    #[derive(Clone)]
    struct FixedShardHasher;

    impl ShardHasher<u32> for FixedShardHasher {
        fn shard_hash(&self, key: &u32) -> u64 {
            u64::from(*key).wrapping_mul(0x9e37_79b9_7f4a_7c15)
        }
    }

    /// Every stored entry as `(key, value, expired)`, sorted, for a full cross-cache
    /// state comparison.
    fn entry_snapshot<H: ShardHasher<u32>>(
        c: &ShardedExpiringCache<u32, Val, H>,
    ) -> Vec<(u32, u32, bool)> {
        let mut out: Vec<(u32, u32, bool)> = Vec::new();
        for shard in c.inner.shards.iter() {
            let guard = shard.lock.read();
            for (k, v) in guard.iter() {
                out.push((*k, v.v, v.expired));
            }
        }
        out.sort_unstable();
        out
    }

    #[test]
    fn evict_is_callable_through_both_entry_points_under_the_key_clone_bound() {
        // The two-phase sweep carries decisions, not cloned keys, so it does not need
        // `K: Clone`. The public bound on the inherent `evict` and on the
        // `ConcurrentCacheEvict` impl is kept anyway: dropping it from the inherent method
        // alone would leave the trait method stricter, and relaxing both is a separate change.
        // A passing test cannot prove a bound is still *required* (that needs a compile-fail
        // harness), so this pins the callable surface: both entry points resolve for a
        // `K: Clone` key and agree on the swept count.
        fn evict_both_ways<K: Clone + Hash + Eq, V: Clone + Expires>(
            c: &ShardedExpiringCache<K, V>,
        ) -> usize {
            let via_inherent = ShardedExpiringCache::evict(c);
            let via_trait = ConcurrentCacheEvict::evict(c);
            via_inherent + via_trait
        }
        let c = ShardedExpiringCache::<u32, Val>::builder().build().unwrap();
        SyncConcurrentCached::cache_set(
            &c,
            1,
            Val {
                v: 10,
                expired: false,
            },
        )
        .unwrap();
        assert_eq!(evict_both_ways(&c), 0, "nothing is expired");
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn cache_set_displaced_expired_entry_counts_land_on_the_owning_shard() {
        // Gap: the displaced-expired-entry branch of cache_set was only exercised on a
        // single default-shard-count cache, asserting only the aggregate. Here 32 distinct
        // keys spread over 8 shards each get an expired entry displaced by cache_set, and
        // every shard's *own* counter (not just the sum) must move by exactly the right
        // amount -- a bug that bumped the wrong shard's counter would still pass an
        // aggregate-only assertion.
        let c = ShardedExpiringCache::<u32, Val>::builder()
            .shards(8)
            .build()
            .unwrap();
        let keys: Vec<u32> = (0..32u32).collect();
        for &k in &keys {
            SyncConcurrentCached::cache_set(
                &c,
                k,
                Val {
                    v: k,
                    expired: true,
                },
            )
            .unwrap();
        }
        let mut expected = vec![0u64; c.shards()];
        for &k in &keys {
            let idx = owning_shard(&c, &k);
            let result = SyncConcurrentCached::cache_set(
                &c,
                k,
                Val {
                    v: k + 1000,
                    expired: false,
                },
            )
            .unwrap();
            assert_eq!(
                result.map(|v| v.v),
                None,
                "displacing an expired entry must return None for key {k}"
            );
            expected[idx] += 1;
        }
        assert_eq!(
            shard_eviction_counters(&c),
            expected,
            "cache_set displacing an expired entry must count on the key's own shard"
        );
        assert!(
            expected.iter().filter(|&&n| n > 0).count() >= 2,
            "the 32 keys must spread over more than one shard: {expected:?}"
        );
        assert_eq!(c.metrics().evictions, Some(expected.iter().sum::<u64>()));
    }

    #[test]
    fn evict_no_callback_zero_expired_shard_counter_stays_exactly_zero() {
        // Gap: the guards are `if removed > 0` / `if !removed.is_empty()`, but nothing
        // asserted a specific *untouched* shard's counter stays exactly 0 while a sibling
        // shard's counter moves -- an aggregate-only assertion cannot catch a spurious
        // per-shard bump. FixedShardHasher pins two disjoint 4-key buckets, one per shard,
        // so shard 1 (all-live) is guaranteed to see zero evictions.
        let c = ShardedExpiringCache::<u32, Val>::builder()
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
        for &k in &buckets[0] {
            SyncConcurrentCached::cache_set(
                &c,
                k,
                Val {
                    v: k,
                    expired: true,
                },
            )
            .unwrap();
        }
        for &k in &buckets[1] {
            SyncConcurrentCached::cache_set(
                &c,
                k,
                Val {
                    v: k,
                    expired: false,
                },
            )
            .unwrap();
        }

        assert_eq!(c.evict(), 4, "only shard 0's entries are expired");
        assert_eq!(
            shard_eviction_counters(&c),
            vec![4, 0],
            "the emptied shard counts four, the untouched shard's counter must be exactly zero"
        );
        assert_eq!(
            c.shard_sizes(),
            vec![0, 4],
            "the all-expired shard is emptied, the none-expired shard is untouched"
        );
    }

    #[test]
    fn evict_callback_and_no_callback_branches_agree_exactly() {
        // Gap: assert the callback (extract_if) and no-callback (retain + length-delta)
        // evict() branches return identical counts and leave identical state for the same
        // input, including a multi-shard mix where some shards have nothing expired.
        let fired: Arc<std::sync::Mutex<Vec<(u32, u32)>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let fired2 = fired.clone();
        let with_cb = ShardedExpiringCache::<u32, Val>::builder()
            .shards(4)
            .hasher(FixedShardHasher)
            .on_evict(move |k: &u32, v: &Val| {
                fired2.lock().expect("callback lock").push((*k, v.v));
            })
            .build()
            .unwrap();
        let no_cb = ShardedExpiringCache::<u32, Val>::builder()
            .shards(4)
            .hasher(FixedShardHasher)
            .build()
            .unwrap();

        // Keys 0..8 expired, 8..24 live -- spread across all 4 shards via FixedShardHasher,
        // with at least one shard ending up with nothing expired.
        for c in [&with_cb, &no_cb] {
            for i in 0..8u32 {
                SyncConcurrentCached::cache_set(
                    c,
                    i,
                    Val {
                        v: i * 10,
                        expired: true,
                    },
                )
                .unwrap();
            }
            for i in 8..24u32 {
                SyncConcurrentCached::cache_set(
                    c,
                    i,
                    Val {
                        v: i * 10,
                        expired: false,
                    },
                )
                .unwrap();
            }
        }
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
        let mut fired_keys = fired.lock().expect("callback lock").clone();
        fired_keys.sort_unstable();
        assert_eq!(
            fired_keys,
            (0..8u32).map(|i| (i, i * 10)).collect::<Vec<_>>(),
            "on_evict must fire once per expired entry"
        );

        // A second, zero-removal sweep: both branches at their empty extreme, no counter
        // moves for either.
        let counters = shard_eviction_counters(&with_cb);
        assert_eq!(with_cb.evict(), 0, "nothing is left to expire");
        assert_eq!(no_cb.evict(), 0, "nothing is left to expire");
        assert_eq!(shard_eviction_counters(&with_cb), counters);
        assert_eq!(shard_eviction_counters(&no_cb), counters);
        assert_eq!(
            fired.lock().expect("callback lock").len(),
            8,
            "a no-op sweep must not fire on_evict"
        );
    }

    #[test]
    fn deep_clone_carries_per_shard_counts_after_a_mix_of_evict_and_cache_remove() {
        // Gap: the author only tested deep_clone after cache_remove-only accrual. Mix
        // evict() (sweeping a third of the entries) with cache_remove_entry (removing half
        // of what's left) before cloning, and confirm every shard's counter -- not just the
        // aggregate -- carries over exactly, and that the clone is independent afterward.
        let c = ShardedExpiringCache::<u32, Val>::builder()
            .shards(8)
            .build()
            .unwrap();
        for i in 0..64u32 {
            SyncConcurrentCached::cache_set(
                &c,
                i,
                Val {
                    v: i,
                    expired: i % 3 == 0,
                },
            )
            .expect("insert must succeed");
        }
        let via_evict = u64::try_from(c.evict()).unwrap();
        assert!(via_evict > 0, "some entries must have been expired");

        let mut removed_via_remove = 0u64;
        for i in (1..64u32).step_by(2) {
            if SyncConcurrentCached::cache_remove_entry(&c, &i)
                .expect("cache_remove_entry must succeed")
                .is_some()
            {
                removed_via_remove += 1;
            }
        }
        let expected_total = via_evict + removed_via_remove;
        let source_counters = shard_eviction_counters(&c);
        assert_eq!(source_counters.iter().sum::<u64>(), expected_total);
        assert_eq!(c.metrics().evictions, Some(expected_total));

        let clone = c.deep_clone();
        assert_eq!(
            shard_eviction_counters(&clone),
            source_counters,
            "deep_clone must carry each shard's counter shard-for-shard after a mix of \
             evict() and cache_remove()"
        );
        assert_eq!(clone.metrics().evictions, Some(expected_total));
        assert_eq!(clone.len(), c.len());

        // The copy is independent: a further eviction on the clone does not move the source.
        SyncConcurrentCached::cache_remove(&clone, &2u32).expect("key must still be present");
        assert_eq!(clone.metrics().evictions, Some(expected_total + 1));
        assert_eq!(c.metrics().evictions, Some(expected_total));
    }

    #[test]
    fn concurrent_evict_cache_remove_and_cache_set_do_not_lose_or_double_count_evictions() {
        // Gap: nothing exercised evict() / cache_remove() / cache_set() hitting different
        // shards simultaneously. Per-shard relaxed loads mean a *mid-flight* aggregate can
        // be torn, so the only things asserted here are conservation identities checked
        // after every worker has joined, not timing-sensitive exact interleavings:
        // (1) on_evict never fires twice for the same stored value (no double-count/double-
        // fire), (2) the total fire count exactly matches the total eviction-counter
        // movement, (3) the raw per-shard sum agrees with metrics(), and (4) no shard is
        // left in a torn state (shard_sizes sums to len()).
        use std::collections::HashSet;
        use std::sync::{Barrier, Mutex};

        const SHARDS: usize = 8;
        const KEYS: u32 = 32;
        const ROUNDS: u32 = 200;
        const WRITERS: usize = 4;
        const REMOVERS: usize = 2;

        let next_id = Arc::new(AtomicU64::new(0));
        let fired_ids: Arc<Mutex<HashSet<u64>>> = Arc::new(Mutex::new(HashSet::new()));
        let fired_count = Arc::new(AtomicU64::new(0));
        let fired_ids2 = fired_ids.clone();
        let fired_count2 = fired_count.clone();
        let c = ShardedExpiringCache::<u32, Val>::builder()
            .shards(SHARDS)
            .on_evict(move |_k: &u32, v: &Val| {
                let mut seen = fired_ids2.lock().expect("fired recorder poisoned");
                assert!(
                    seen.insert(u64::from(v.v)),
                    "on_evict fired twice for the same stored value {} -- double-fire under \
                     concurrent evict/cache_remove/cache_set",
                    v.v
                );
                fired_count2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();
        let evictions_before = c.metrics().evictions.unwrap();

        let gate = Arc::new(Barrier::new(WRITERS + REMOVERS + 1));
        let mut handles = Vec::new();

        for _ in 0..WRITERS {
            let c = c.clone();
            let gate = gate.clone();
            let next_id = next_id.clone();
            handles.push(std::thread::spawn(move || {
                gate.wait();
                for r in 0..ROUNDS {
                    let k = r % KEYS;
                    // Unique id per stored value lets on_evict distinguish "this exact
                    // stored entry" from "a same-key entry that replaced it".
                    let id =
                        u32::try_from(next_id.fetch_add(1, Ordering::Relaxed) % 1_000_000).unwrap();
                    // A third of inserts are born already-expired, so cache_set's
                    // displaced-expired-entry branch races cache_remove and evict() for
                    // the very same entries.
                    let expired = id % 3 == 0;
                    let _ = SyncConcurrentCached::cache_set(&c, k, Val { v: id, expired }).unwrap();
                }
            }));
        }
        for _ in 0..REMOVERS {
            let c = c.clone();
            let gate = gate.clone();
            handles.push(std::thread::spawn(move || {
                gate.wait();
                for r in 0..ROUNDS {
                    let k = r % KEYS;
                    let _ = SyncConcurrentCached::cache_remove(&c, &k).unwrap();
                }
            }));
        }
        {
            let c = c.clone();
            let gate = gate.clone();
            handles.push(std::thread::spawn(move || {
                gate.wait();
                for _ in 0..ROUNDS {
                    let _ = c.evict();
                }
            }));
        }

        for h in handles {
            h.join().expect("worker thread must not panic");
        }

        let fired = fired_count.load(Ordering::Relaxed);
        let evictions_after = c.metrics().evictions.unwrap();
        assert_eq!(
            fired,
            evictions_after - evictions_before,
            "on_evict must fire exactly once per counted eviction across the whole race, no \
             matter how evict()/cache_remove()/cache_set() interleaved"
        );
        assert_eq!(
            shard_eviction_counters(&c).iter().sum::<u64>(),
            evictions_after,
            "the raw per-shard counters must sum to the same total metrics() reports"
        );
        assert_eq!(
            ConcurrentCacheBase::cache_evictions(&c),
            Some(evictions_after),
            "cache_evictions() must agree with metrics() after the race"
        );
        assert_eq!(
            c.shard_sizes().iter().sum::<usize>(),
            c.len(),
            "post-race shard sizes must still sum to the total length (no torn shard)"
        );
    }
}
