use std::hash::Hash;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Encode a TTL into the `ttl_nanos` atomic. A zero duration encodes as `0`
/// (expiry disabled / no expiry).
#[inline]
fn encode_ttl(ttl: Duration) -> u64 {
    ttl.as_nanos().min(u64::MAX as u128) as u64
}

/// Decode the `ttl_nanos` atomic into an optional TTL. `0` means expiry is
/// disabled (entries never expire), so it decodes to `None`.
#[inline]
fn decode_ttl(nanos: u64) -> Option<Duration> {
    if nanos == 0 {
        None
    } else {
        Some(Duration::from_nanos(nanos))
    }
}

#[cfg(feature = "ahash")]
use ahash::RandomState;
#[cfg(not(feature = "ahash"))]
use std::collections::hash_map::RandomState;

use std::collections::HashMap;

#[cfg(feature = "async_core")]
use crate::ConcurrentCachedAsync;
use crate::time::{Duration, Instant};
use crate::{
    CacheMetrics, ConcurrentCacheBase, ConcurrentCacheEvict, ConcurrentCacheTtl, ConcurrentCached,
    ConcurrentCloneCached,
};

use super::{
    CachePadded, DefaultShardHasher, Shard, ShardHasher, checked_shard_count, shard_index,
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
    evictions: AtomicU64,
}

/// A fully-concurrent, partitioned, TTL-bounded in-memory cache.
///
/// Wraps an `Arc` — `clone()` is an Arc-share (shared state), not a deep copy.
/// Use [`deep_clone`](ShardedTtlCacheBase::deep_clone) to get an independent copy.
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
/// This is a type alias for `ShardedTtlCacheBase<K, V, DefaultShardHasher>`.
/// To use a custom shard hasher, call [`ShardedTtlCache::builder()`] and then
/// [`hasher`](ShardedTtlCacheBuilder::hasher), which yields a `ShardedTtlCacheBase<K, V, H>`
/// over your hasher.
pub type ShardedTtlCache<K, V> = ShardedTtlCacheBase<K, V, DefaultShardHasher>;

/// Backing type for [`ShardedTtlCache`] with a generic shard hasher `H`.
pub struct ShardedTtlCacheBase<K, V, H = DefaultShardHasher> {
    inner: Arc<TtlInner<K, V, H>>,
}

impl<K, V, H> Clone for ShardedTtlCacheBase<K, V, H> {
    /// Arc-share clone — both handles point to the same underlying cache.
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<K, V, H> std::fmt::Debug for ShardedTtlCacheBase<K, V, H> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ttl = self.ttl_duration_impl();
        f.debug_struct("ShardedTtlCache")
            .field("shards", &self.inner.shards.len())
            .field("ttl", &ttl)
            .finish_non_exhaustive()
    }
}

impl<K, V, H> ShardedTtlCacheBase<K, V, H> {
    /// Resolve the currently configured TTL, independent of hasher bounds.
    ///
    /// Returns `None` when expiry is disabled (entries never expire), otherwise
    /// `Some(ttl)`.
    #[inline]
    fn ttl_duration_impl(&self) -> Option<Duration> {
        decode_ttl(self.inner.ttl_nanos.load(Ordering::Relaxed))
    }
}

impl<K, V> ShardedTtlCacheBase<K, V, DefaultShardHasher>
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
    /// builder's hasher type and `build` then yields a `ShardedTtlCacheBase` over that hasher.
    /// `new` and `builder` exist only on the default-hasher alias, so a custom hasher is always
    /// introduced via `hasher`, never a `ShardedTtlCacheBase::<_, _, H>` turbofish.
    #[must_use]
    pub fn builder() -> ShardedTtlCacheBuilder<K, V, DefaultShardHasher> {
        ShardedTtlCacheBuilder::default()
    }
}

impl<K, V, H> ShardedTtlCacheBase<K, V, H>
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
        // `expires_at = None` means never-expires (TTL was disabled at insert time).
        match entry.expires_at {
            None => false,
            Some(t) => Instant::now() >= t,
        }
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

impl<K: Clone + Hash + Eq, V: Clone, H: ShardHasher<K>> ShardedTtlCacheBase<K, V, H> {
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
            inner: Arc::new(TtlInner {
                shards,
                shard_mask: self.inner.shard_mask,
                hasher: self.inner.hasher.clone(),
                on_evict: self.inner.on_evict.clone(),
                ttl_nanos: AtomicU64::new(self.inner.ttl_nanos.load(Ordering::Relaxed)),
                refresh: AtomicBool::new(self.inner.refresh.load(Ordering::Relaxed)),
                evictions: AtomicU64::new(self.inner.evictions.load(Ordering::Relaxed)),
            }),
        }
    }
}

impl<K, V, H: ShardHasher<K>> ShardedTtlCacheBase<K, V, H>
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

impl<K, V, H: ShardHasher<K>> ShardedTtlCacheBase<K, V, H>
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
        for shard in self.inner.shards.iter() {
            let removed: Vec<(K, TimedEntry<V>)> = shard.lock.write().drain().collect();
            if !removed.is_empty() {
                self.inner
                    .evictions
                    .fetch_add(removed.len() as u64, Ordering::Relaxed);
                if let Some(on_evict) = &self.inner.on_evict {
                    for (k, entry) in &removed {
                        on_evict(k, &entry.value);
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
        let now = Instant::now();
        for shard in self.inner.shards.iter() {
            let removed = {
                let mut guard = shard.lock.write();
                let expired_keys: Vec<K> = guard
                    .iter()
                    // An entry is expired when expires_at is Some(t) and now >= t.
                    // None means never-expires.
                    .filter(|(_, e)| e.expires_at.is_some_and(|t| now >= t))
                    .map(|(k, _)| k.clone())
                    .collect();
                let mut removed = Vec::new();
                for k in expired_keys {
                    if let Some((key, entry)) = guard.remove_entry(&k) {
                        removed.push((key, entry));
                    }
                }
                removed
            };

            total += removed.len();
            if !removed.is_empty() {
                self.inner
                    .evictions
                    .fetch_add(removed.len() as u64, Ordering::Relaxed);
                if let Some(cb) = &self.inner.on_evict {
                    for (k, entry) in &removed {
                        cb(k, &entry.value);
                    }
                }
            }
        }
        total
    }

    // ---- Inherent `&self` TTL knobs ----

    /// Return the current TTL.
    #[must_use]
    pub fn ttl(&self) -> Option<Duration> {
        self.ttl_duration()
    }

    /// Set the TTL applied to entries inserted after this call, returning the previous value.
    ///
    /// The new TTL only affects entries inserted after the change; existing entries keep their
    /// original expiry. Note that entries read while `refresh_on_hit` is enabled re-anchor to
    /// the TTL current at access time.
    ///
    /// TTL values longer than approximately 584 years are silently clamped to `u64::MAX`
    /// nanoseconds (~584 years). In practice this limit is never reached.
    ///
    /// A zero `ttl` disables expiry — it is exactly equivalent to
    /// [`unset_ttl`](Self::unset_ttl), and subsequently inserted entries never expire.
    pub fn set_ttl(&self, ttl: Duration) -> Option<Duration> {
        let prev = self
            .inner
            .ttl_nanos
            .swap(encode_ttl(ttl), Ordering::Relaxed);
        decode_ttl(prev)
    }

    /// Remove the TTL (entries never expire after this point).
    pub fn unset_ttl(&self) -> Option<Duration> {
        let prev = self.inner.ttl_nanos.swap(0, Ordering::Relaxed);
        decode_ttl(prev)
    }

    /// Set whether cache hits refresh the TTL of the accessed entry,
    /// returning the previous value.
    pub fn set_refresh_on_hit(&self, refresh: bool) -> bool {
        self.inner.refresh.swap(refresh, Ordering::Relaxed)
    }

    /// Return whether cache hits refresh the TTL.
    #[must_use]
    pub fn refresh_on_hit(&self) -> bool {
        self.inner.refresh.load(Ordering::Relaxed)
    }
}

impl<K, V, H> ConcurrentCacheEvict for ShardedTtlCacheBase<K, V, H>
where
    K: Hash + Eq + Clone,
    H: ShardHasher<K>,
{
    fn evict(&self) -> usize {
        ShardedTtlCacheBase::evict(self)
    }
}

impl<K, V, H> ConcurrentCacheBase for ShardedTtlCacheBase<K, V, H>
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
        Some(self.inner.evictions.load(Ordering::Relaxed))
    }
}

impl<K, V, H> ConcurrentCacheTtl for ShardedTtlCacheBase<K, V, H>
where
    K: Hash + Eq,
    V: Clone,
    H: ShardHasher<K>,
{
    fn ttl(&self) -> Option<Duration> {
        self.ttl_duration()
    }

    fn set_ttl(&self, ttl: Duration) -> Option<Duration> {
        ShardedTtlCacheBase::set_ttl(self, ttl)
    }

    fn unset_ttl(&self) -> Option<Duration> {
        ShardedTtlCacheBase::unset_ttl(self)
    }

    fn refresh_on_hit(&self) -> bool {
        self.inner.refresh.load(Ordering::Relaxed)
    }

    fn set_refresh_on_hit(&self, refresh: bool) -> bool {
        self.inner.refresh.swap(refresh, Ordering::Relaxed)
    }
}

impl<K, V, H> ConcurrentCached<K, V> for ShardedTtlCacheBase<K, V, H>
where
    K: Hash + Eq,
    V: Clone,
    H: ShardHasher<K>,
{
    fn cache_get(&self, k: &K) -> Result<Option<V>, Self::Error> {
        let shard = self.shard_of(k);
        if self.inner.refresh.load(Ordering::Relaxed) {
            let mut guard = shard.lock.write();
            match guard.get_mut(k) {
                Some(entry) if !self.is_expired(entry) => {
                    let now = Instant::now();
                    entry.expires_at = self.compute_expires_at(now).or(entry.expires_at);
                    let value = Some(entry.value.clone());
                    drop(guard);
                    shard.hits.fetch_add(1, Ordering::Relaxed);
                    return Ok(value);
                }
                Some(_) => {
                    let removed = guard.remove_entry(k);
                    drop(guard);
                    if let Some((stored_k, entry)) = removed {
                        self.inner.evictions.fetch_add(1, Ordering::Relaxed);
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
        let (expired, value) = {
            let guard = shard.lock.read();
            match guard.get(k) {
                None => {
                    shard.misses.fetch_add(1, Ordering::Relaxed);
                    return Ok(None);
                }
                Some(entry) => {
                    let expired = self.is_expired(entry);
                    let value = if !expired {
                        Some(entry.value.clone())
                    } else {
                        None
                    };
                    (expired, value)
                }
            }
        };
        if expired {
            // Upgrade to write lock to remove the expired entry.
            let mut guard = shard.lock.write();
            // Re-check under write lock — another thread may have replaced the entry
            // with a fresh value in the meantime; clone it out in the same lookup.
            let fresh_value = match guard.get(k) {
                Some(entry) if !self.is_expired(entry) => Some(entry.value.clone()),
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
                self.inner.evictions.fetch_add(1, Ordering::Relaxed);
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
        // the check). When an `on_evict` callback is configured, remove-then-insert so the
        // owned old key can fire the callback after the lock is released (on_evict-after-unlock).
        let old: Option<(Option<K>, TimedEntry<V>, bool)> = if self.inner.on_evict.is_some() {
            let mut guard = shard.lock.write();
            let removed = guard.remove_entry(&k);
            guard.insert(k, new_entry);
            removed.map(|(ok, e)| {
                let expired = e.expires_at.is_some_and(|t| Instant::now() >= t);
                (Some(ok), e, expired)
            })
        } else {
            shard.lock.write().insert(k, new_entry).map(|e| {
                let expired = e.expires_at.is_some_and(|t| Instant::now() >= t);
                (None, e, expired)
            })
        };
        match old {
            // A displaced expired value is filtered from the return (matching cache_remove and
            // the single-owner TTL stores); fire on_evict and count an eviction for it.
            Some((key, entry, true)) => {
                if let (Some(cb), Some(key)) = (&self.inner.on_evict, &key) {
                    cb(key, &entry.value);
                }
                self.inner.evictions.fetch_add(1, Ordering::Relaxed);
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
            self.inner.evictions.fetch_add(1, Ordering::Relaxed);
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
            self.inner.evictions.fetch_add(1, Ordering::Relaxed);
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
        }
        self.inner.evictions.store(0, Ordering::Relaxed);
        Ok(())
    }
}

#[cfg(feature = "async_core")]
impl<K, V, H> ConcurrentCachedAsync<K, V> for ShardedTtlCacheBase<K, V, H>
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

/// Builder for [`ShardedTtlCacheBase`].
///
/// Unlike the LRU-bounded builders, `ShardedTtlCacheBuilder` has no `per_shard_max_size` method
/// because `ShardedTtlCache` is unbounded in size — entries expire by TTL, not by capacity.
pub struct ShardedTtlCacheBuilder<K, V, H = DefaultShardHasher> {
    shards: Option<usize>,
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
            ttl: None,
            refresh: false,
            hasher: Some(DefaultShardHasher::default()),
            on_evict: None,
            _k: std::marker::PhantomData,
            _v: std::marker::PhantomData,
        }
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
    /// found and removed; explicitly via [`evict`](ShardedTtlCacheBase::evict); on
    /// explicit [`cache_remove`](ConcurrentCached::cache_remove); on
    /// [`cache_remove_entry`](ConcurrentCached::cache_remove_entry); and on
    /// [`cache_set`](ConcurrentCached::cache_set) when the displaced entry is already expired.
    /// Does **not** fire on [`clear`](ShardedTtlCacheBase::clear);
    /// use [`cache_clear_with_on_evict`](ShardedTtlCacheBase::cache_clear_with_on_evict) to opt in.
    /// [`cache_clear_with_on_evict`](ShardedTtlCacheBase::cache_clear_with_on_evict) fires
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
    /// Use [`ShardedTtlCache::builder()`] (or [`ShardedTtlCacheBase::builder()`]) to obtain a
    /// builder, set at least [`ttl`](Self::ttl), then call `.build()`.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError::MissingRequired`] if `ttl` was not set,
    /// [`BuildError::InvalidValue`] if the TTL is zero, or [`BuildError`] if the
    /// shard count overflows.
    #[must_use = "the Result from build() must be used"]
    pub fn build(self) -> Result<ShardedTtlCacheBase<K, V, H>, BuildError>
    where
        K: Hash + Eq,
        H: ShardHasher<K>,
    {
        let ttl = self.ttl.ok_or(BuildError::MissingRequired("ttl"))?;
        crate::stores::validate_ttl(ttl)?;
        let n = checked_shard_count(self.shards)?;
        let mask = n - 1;
        let shards = (0..n)
            .map(|_| CachePadded(Shard::new(HashMap::with_hasher(RandomState::new()))))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Ok(ShardedTtlCacheBase {
            inner: Arc::new(TtlInner {
                shards,
                shard_mask: mask,
                hasher: self
                    .hasher
                    .expect("hasher is always initialized via Default or .hasher()"),
                on_evict: self.on_evict,
                ttl_nanos: AtomicU64::new(encode_ttl(ttl)),
                refresh: AtomicBool::new(self.refresh),
                evictions: AtomicU64::new(0),
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
        existing: &ShardedTtlCacheBase<K, V, H2>,
    ) -> Result<ShardedTtlCacheBase<K, V, H>, BuildError>
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

impl<K, V, H> ConcurrentCloneCached<K, V> for ShardedTtlCacheBase<K, V, H>
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
                    let expired = self.is_expired(entry);
                    let value = entry.value.clone();
                    if !expired {
                        let now = Instant::now();
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
                    let expired = self.is_expired(entry);
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
        let c = ShardedTtlCacheBase::<u32, u32>::builder()
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
        let new_cache = ShardedTtlCacheBase::<u32, u32>::builder()
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
        let new_cache = ShardedTtlCacheBase::<u32, u32>::builder()
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
        let err = ShardedTtlCacheBase::<u32, u32>::builder()
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
        let c = ShardedTtlCacheBase::<u32, u32>::builder()
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
        let c = ShardedTtlCacheBase::<u32, u32>::builder()
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
        let c = ShardedTtlCacheBase::<u32, u32>::builder()
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
        let c = ShardedTtlCacheBase::<u32, u32>::builder()
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
        let c = ShardedTtlCacheBase::<u32, u32>::builder()
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
        let c = ShardedTtlCacheBase::<u32, u32>::builder()
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
        let c = ShardedTtlCacheBase::<u32, u32>::builder()
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
        let c = ShardedTtlCacheBase::<u32, u32>::builder()
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
        let c = ShardedTtlCacheBase::<u32, u32>::builder()
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
        let c = ShardedTtlCacheBase::<u32, u32>::builder()
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
}
