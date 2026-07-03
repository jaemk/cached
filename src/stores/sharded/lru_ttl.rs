use std::hash::Hash;
use std::marker::PhantomData;
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
use crate::stores::{BuildError, HasEvict, LruCache, NoEvict, TimedEntry};
use crate::{Cached, CachedIter, CachedPeek};

type OnEvict<K, V> = Arc<dyn Fn(&K, &V) + Send + Sync>;

#[allow(clippy::type_complexity)]
struct LruTtlInner<K, V, H> {
    shards: Box<[CachePadded<Shard<LruCache<K, TimedEntry<V>>>>]>,
    shard_mask: usize,
    hasher: H,
    on_evict: Option<OnEvict<K, V>>,
    /// TTL in nanoseconds, or `0` to mean expiry is disabled (entries never expire).
    /// A zero stored value is the single sentinel for "no expiry"; there is no separate
    /// `ttl_set` flag. `unset_ttl`/`set_ttl(0)` store `0`; `set_ttl(nonzero)` stores the ttl.
    ttl_nanos: AtomicU64,
    refresh: AtomicBool,
    /// Evictions not driven by LRU capacity pressure: TTL expiry (via [`evict`](ShardedLruTtlCacheBase::evict)),
    /// explicit removes ([`cache_remove`](ConcurrentCached::cache_remove) /
    /// [`cache_remove_entry`](ConcurrentCached::cache_remove_entry)), and
    /// [`cache_clear_with_on_evict`](ShardedLruTtlCacheBase::cache_clear_with_on_evict).
    /// LRU capacity evictions are tracked per-shard in the inner `LruCache`.
    non_capacity_evictions: AtomicU64,
    total_capacity: usize,
}

/// A fully-concurrent, partitioned, LRU-bounded, TTL-expiring in-memory cache.
///
/// Wraps an `Arc` — `clone()` is an Arc-share (shared state), not a deep copy.
/// Use [`deep_clone`](ShardedLruTtlCacheBase::deep_clone) to get an independent copy.
///
/// **Note**: `K` and `V` must implement `Clone` (`K` for LRU key tracking; `V` because reads
/// return owned values cloned from under the shard lock).
///
/// This is a type alias for `ShardedLruTtlCacheBase<K, V, DefaultShardHasher>`.
/// To use a custom shard hasher, call [`ShardedLruTtlCache::builder()`] and then
/// [`hasher`](ShardedLruTtlCacheBuilder::hasher), which yields a
/// `ShardedLruTtlCacheBase<K, V, H>` over your hasher.
///
/// **Note**: LRU promotion requires mutable access to the per-shard store, so
/// `cache_get` acquires a **write** lock (unlike `ShardedTtlCache` which only needs a read lock
/// when `refresh_on_hit` is disabled). Under many concurrent readers this can be a bottleneck;
/// consider `ShardedTtlCache` if you do not need capacity bounding.
///
/// **Note**: `K` must implement `Clone` (needed for LRU key tracking). `ShardedTtlCache<K, V>`
/// requires only `K: Hash + Eq`.
///
/// **Note**: Setting an `on_evict` callback transitions the builder to requiring `'static` bounds
/// on `K` and `V` due to internal closure wrapping. If you have non-`'static` keys or values,
/// do not configure an `on_evict` callback.
///
/// **`len` / `evict` contract**: `len()` (the inherent method) returns the raw stored entry
/// count across all shards and may include expired-but-not-yet-swept entries. Call `evict()`
/// (via [`ConcurrentCacheEvict`](crate::ConcurrentCacheEvict)) to physically remove expired
/// entries and obtain an accurate live count. Sharded stores do not implement `CachedIter`.
pub type ShardedLruTtlCache<K, V> = ShardedLruTtlCacheBase<K, V, DefaultShardHasher>;

/// Backing type for [`ShardedLruTtlCache`] with a generic shard hasher `H`.
pub struct ShardedLruTtlCacheBase<K, V, H = DefaultShardHasher> {
    inner: Arc<LruTtlInner<K, V, H>>,
}

impl<K, V, H> Clone for ShardedLruTtlCacheBase<K, V, H> {
    /// Arc-share clone — both handles point to the same underlying cache.
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<K, V, H> ShardedLruTtlCacheBase<K, V, H> {
    /// Resolve the currently configured TTL, independent of hasher bounds.
    ///
    /// Returns `None` when expiry is disabled (entries never expire), otherwise
    /// `Some(ttl)`.
    #[inline]
    fn ttl_duration_impl(&self) -> Option<Duration> {
        decode_ttl(self.inner.ttl_nanos.load(Ordering::Relaxed))
    }
}

impl<K, V, H> std::fmt::Debug for ShardedLruTtlCacheBase<K, V, H> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ttl = self.ttl_duration_impl();
        f.debug_struct("ShardedLruTtlCache")
            .field("shards", &self.inner.shards.len())
            .field("capacity", &self.inner.total_capacity)
            .field("ttl", &ttl)
            .finish_non_exhaustive()
    }
}

impl<K, V> ShardedLruTtlCacheBase<K, V, DefaultShardHasher>
where
    K: Hash + Eq + Clone,
{
    /// Construct a ready-to-use [`ShardedLruTtlCache`] holding up to roughly `max_size`
    /// entries total with the given `ttl`, the [`DefaultShardHasher`], and a default shard
    /// count.
    ///
    /// Note that the effective total capacity can exceed `max_size` for small values
    /// because each shard reserves a minimum capacity (see
    /// [`max_size`](ShardedLruTtlCacheBuilder::max_size)). For a custom hasher, shard count,
    /// per-shard cap, `refresh_on_hit`, or `on_evict`, use [`builder`](Self::builder).
    ///
    /// # Panics
    ///
    /// Panics if `max_size` is `0`, if `ttl` is zero, or if the effective sharded capacity
    /// overflows `usize` / a per-shard allocation fails. Use [`builder`](Self::builder) with
    /// [`build`](ShardedLruTtlCacheBuilder::build) to handle those cases without panicking.
    #[must_use]
    pub fn new(max_size: usize, ttl: Duration) -> ShardedLruTtlCache<K, V> {
        Self::builder()
            .max_size(max_size)
            .ttl(ttl)
            .build()
            .expect("ShardedLruTtlCache::new requires a non-zero max_size and non-zero ttl")
    }

    /// Return a builder for constructing a [`ShardedLruTtlCache`].
    ///
    /// The builder starts with the [`DefaultShardHasher`]. To use a custom hasher, call
    /// [`hasher`](ShardedLruTtlCacheBuilder::hasher) on the returned builder; it switches the
    /// builder's hasher type and `build` then yields a `ShardedLruTtlCacheBase` over that
    /// hasher. `new` and `builder` exist only on the default-hasher alias, so a custom hasher
    /// is always introduced via `hasher`, never a `ShardedLruTtlCacheBase::<_, _, H>` turbofish.
    #[must_use]
    pub fn builder() -> ShardedLruTtlCacheBuilder<K, V> {
        ShardedLruTtlCacheBuilder::default()
    }
}

impl<K, V, H> ShardedLruTtlCacheBase<K, V, H>
where
    K: Hash + Eq + Clone,
    H: ShardHasher<K>,
{
    #[inline]
    fn shard_of(&self, k: &K) -> &CachePadded<Shard<LruCache<K, TimedEntry<V>>>> {
        let h = self.inner.hasher.shard_hash(k);
        &self.inner.shards[shard_index(h, self.inner.shard_mask)]
    }

    #[inline]
    fn ttl_duration(&self) -> Option<Duration> {
        self.ttl_duration_impl()
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

impl<K: Clone + Hash + Eq, V: Clone, H: ShardHasher<K>> ShardedLruTtlCacheBase<K, V, H> {
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
            inner: Arc::new(LruTtlInner {
                shards,
                shard_mask: self.inner.shard_mask,
                hasher: self.inner.hasher.clone(),
                on_evict: self.inner.on_evict.clone(),
                ttl_nanos: AtomicU64::new(self.inner.ttl_nanos.load(Ordering::Relaxed)),
                refresh: AtomicBool::new(self.inner.refresh.load(Ordering::Relaxed)),
                non_capacity_evictions: AtomicU64::new(
                    self.inner.non_capacity_evictions.load(Ordering::Relaxed),
                ),
                total_capacity: self.inner.total_capacity,
            }),
        }
    }
}

impl<K, V, H: ShardHasher<K>> ShardedLruTtlCacheBase<K, V, H>
where
    K: Hash + Eq + Clone,
    V: Clone,
{
    /// Retrieve a cached value, returning `None` on a miss or if the entry has expired.
    ///
    /// This is the infallible ergonomic API for the concrete type. Generic code over
    /// [`ConcurrentCached`] should use the `Result`-returning trait methods (`cache_get` or the
    /// trait's `get` alias), callable as `ConcurrentCached::get(&store, k)` when this inherent
    /// method is in scope.
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

impl<K, V, H: ShardHasher<K>> ShardedLruTtlCacheBase<K, V, H>
where
    K: Hash + Eq + Clone,
{
    /// Return aggregate metrics across all shards. Evictions include LRU
    /// capacity evictions (per-shard), TTL-expiry evictions, and explicit
    /// [`cache_remove`](ConcurrentCached::cache_remove) calls.
    ///
    /// Note: the `size` field includes entries that have expired but not yet been
    /// swept by [`evict`](Self::evict). Call `evict()` first for an accurate live count.
    /// `capacity` reflects the effective total capacity — may exceed the requested
    /// `size` when the 16-per-shard minimum floor is applied; see [`capacity`](Self::capacity).
    #[must_use]
    pub fn metrics(&self) -> CacheMetrics {
        let mut hits = 0u64;
        let mut misses = 0u64;
        let mut lru_evictions = 0u64;
        let mut size = 0usize;
        for shard in self.inner.shards.iter() {
            hits += shard.hits.load(Ordering::Relaxed);
            misses += shard.misses.load(Ordering::Relaxed);
            let guard = shard.lock.read();
            if let Some(e) = guard.cache_evictions() {
                lru_evictions += e;
            }
            size += guard.cache_size();
        }
        CacheMetrics {
            hits: Some(hits),
            misses: Some(misses),
            evictions: Some(
                lru_evictions + self.inner.non_capacity_evictions.load(Ordering::Relaxed),
            ),
            entry_count: Some(size),
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
    /// Unlike [`clear`](Self::clear), every removed entry is counted as an eviction
    /// (`metrics().evictions`) whether or not an `on_evict` callback is configured; the callback
    /// fires only when one is set.
    pub fn cache_clear_with_on_evict(&self) {
        for shard in self.inner.shards.iter() {
            let removed: Vec<(K, TimedEntry<V>)> = {
                let mut guard = shard.lock.write();
                let keys: Vec<K> = guard.iter().map(|(k, _)| k.clone()).collect();
                let mut removed = Vec::with_capacity(keys.len());
                for k in keys {
                    if let Some(pair) = guard.pop_raw(&k) {
                        removed.push(pair);
                    }
                }
                removed
            };
            if !removed.is_empty() {
                self.inner
                    .non_capacity_evictions
                    .fetch_add(removed.len() as u64, Ordering::Relaxed);
                if let Some(on_evict) = &self.inner.on_evict {
                    for (k, entry) in &removed {
                        on_evict(k, &entry.value);
                    }
                }
            }
        }
    }

    /// Effective total capacity across all shards.
    ///
    /// When constructed with [`max_size`](ShardedLruTtlCacheBuilder::max_size), this may
    /// be larger than the requested size because per-shard capacity is rounded
    /// up with ceiling division.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.inner.total_capacity
    }

    /// Sweep all shards for expired entries, remove them, fire the `on_evict` callback
    /// (if set) for each, and return the total count of removed entries.
    #[must_use]
    pub fn evict(&self) -> usize {
        let mut total = 0;
        let now = Instant::now();
        for shard in self.inner.shards.iter() {
            let removed = {
                let mut guard = shard.lock.write();
                let expired: Vec<K> = guard
                    .iter()
                    // An entry is expired when expires_at is Some(t) and now >= t.
                    // None means never-expires.
                    .filter(|(_, e)| e.expires_at.is_some_and(|t| now >= t))
                    .map(|(k, _)| k.clone())
                    .collect();
                let mut removed = Vec::new();
                for k in expired {
                    // Use pop_raw (not cache_remove) to avoid double-counting:
                    // the outer evict() handles on_evict and non_capacity_evictions itself.
                    if let Some((key, entry)) = guard.pop_raw(&k) {
                        removed.push((key, entry));
                    }
                }
                removed
            };

            total += removed.len();
            if !removed.is_empty() {
                self.inner
                    .non_capacity_evictions
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

    /// Set the TTL used when checking existing and newly inserted entries, returning the previous value.
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

impl<K, V, H> ConcurrentCacheEvict for ShardedLruTtlCacheBase<K, V, H>
where
    K: Hash + Eq + Clone,
    H: ShardHasher<K>,
{
    fn evict(&self) -> usize {
        ShardedLruTtlCacheBase::evict(self)
    }
}

impl<K, V, H> ConcurrentCacheBase for ShardedLruTtlCacheBase<K, V, H>
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
        Some(self.inner.total_capacity)
    }

    fn cache_evictions(&self) -> Option<u64> {
        let mut lru_evictions = 0u64;
        for shard in self.inner.shards.iter() {
            let guard = shard.lock.read();
            if let Some(e) = Cached::cache_evictions(&*guard) {
                lru_evictions += e;
            }
        }
        Some(lru_evictions + self.inner.non_capacity_evictions.load(Ordering::Relaxed))
    }
}

impl<K, V, H> ConcurrentCacheTtl for ShardedLruTtlCacheBase<K, V, H>
where
    K: Hash + Eq + Clone,
    V: Clone,
    H: ShardHasher<K>,
{
    fn ttl(&self) -> Option<Duration> {
        self.ttl_duration()
    }

    fn set_ttl(&self, ttl: Duration) -> Option<Duration> {
        ShardedLruTtlCacheBase::set_ttl(self, ttl)
    }

    fn unset_ttl(&self) -> Option<Duration> {
        ShardedLruTtlCacheBase::unset_ttl(self)
    }

    fn refresh_on_hit(&self) -> bool {
        self.inner.refresh.load(Ordering::Relaxed)
    }

    fn set_refresh_on_hit(&self, refresh: bool) -> bool {
        self.inner.refresh.swap(refresh, Ordering::Relaxed)
    }
}

impl<K, V, H> ConcurrentCached<K, V> for ShardedLruTtlCacheBase<K, V, H>
where
    K: Hash + Eq + Clone,
    V: Clone,
    H: ShardHasher<K>,
{
    fn cache_get(&self, k: &K) -> Result<Option<V>, Self::Error> {
        let shard = self.shard_of(k);
        let refresh = self.inner.refresh.load(Ordering::Relaxed);

        let mut guard = shard.lock.write();

        // Peek first (no LRU promotion) to check expiry before committing recency.
        // This avoids promoting entries that will be immediately evicted as expired.
        let expired = match guard.cache_peek(k) {
            None => {
                shard.misses.fetch_add(1, Ordering::Relaxed);
                return Ok(None);
            }
            // expired = None (never-expires) -> false; Some(t) -> expired if now >= t
            Some(entry) => entry.expires_at.is_some_and(|t| Instant::now() >= t),
        };

        if expired {
            // Use pop_raw (bypasses on_evict, unlike cache_remove_entry); we fire
            // on_evict manually below after releasing the shard lock.
            let removed = guard.pop_raw(k);
            drop(guard);
            if let Some((ref ek, ref entry)) = removed {
                if let Some(cb) = &self.inner.on_evict {
                    cb(ek, &entry.value);
                }
                self.inner
                    .non_capacity_evictions
                    .fetch_add(1, Ordering::Relaxed);
            }
            shard.misses.fetch_add(1, Ordering::Relaxed);
            return Ok(None);
        }

        // Live hit — update LRU recency and extract value.
        // Use a single mutable access when refresh is enabled to avoid double
        // LRU promotion and double-incrementing LruCache's internal hit counter.
        let value = if refresh {
            guard.cache_get_mut(k).map(|e| {
                let now = Instant::now();
                e.expires_at = self.compute_expires_at(now).or(e.expires_at);
                e.value.clone()
            })
        } else {
            guard.cache_get(k).map(|e| e.value.clone())
        };
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
        // Capture the displaced entry. When an `on_evict` callback is configured, pop-then-set
        // (`pop_raw` is silent and returns the owned key) so the callback can fire after the lock
        // is released; otherwise a plain set. The entry count is unchanged, so no capacity
        // eviction is triggered by the re-insert.
        let old: Option<(Option<K>, TimedEntry<V>)> = if self.inner.on_evict.is_some() {
            let mut guard = shard.lock.write();
            let removed = guard.pop_raw(&k);
            guard.cache_set(k, new_entry);
            removed.map(|(ok, e)| (Some(ok), e))
        } else {
            shard
                .lock
                .write()
                .cache_set(k, new_entry)
                .map(|e| (None, e))
        };
        match old {
            // A displaced expired value is filtered from the return (matching cache_remove and
            // the single-owner TTL stores); fire on_evict and count an eviction for it.
            Some((key, entry)) if entry.expires_at.is_some_and(|t| Instant::now() >= t) => {
                if let (Some(on_evict), Some(key)) = (&self.inner.on_evict, &key) {
                    on_evict(key, &entry.value);
                }
                self.inner
                    .non_capacity_evictions
                    .fetch_add(1, Ordering::Relaxed);
                Ok(None)
            }
            Some((_, entry)) => Ok(Some(entry.value)),
            None => Ok(None),
        }
    }

    fn cache_remove(&self, k: &K) -> Result<Option<V>, Self::Error> {
        let shard = self.shard_of(k);
        let removed = shard.lock.write().pop_raw(k);
        if let Some((key, entry)) = removed {
            self.inner
                .non_capacity_evictions
                .fetch_add(1, Ordering::Relaxed);
            if let Some(on_evict) = &self.inner.on_evict {
                on_evict(&key, &entry.value);
            }
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
        let removed = shard.lock.write().pop_raw(k);
        if let Some((ref stored_k, ref entry)) = removed {
            self.inner
                .non_capacity_evictions
                .fetch_add(1, Ordering::Relaxed);
            if let Some(on_evict) = &self.inner.on_evict {
                on_evict(stored_k, &entry.value);
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
            // Zero the per-shard inner store's metrics, including its LRU capacity-eviction counter.
            shard.lock.write().cache_reset_metrics();
        }
        self.inner
            .non_capacity_evictions
            .store(0, Ordering::Relaxed);
        Ok(())
    }
}

#[cfg(feature = "async_core")]
impl<K, V, H> ConcurrentCachedAsync<K, V> for ShardedLruTtlCacheBase<K, V, H>
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
}

/// Builder for [`ShardedLruTtlCacheBase`].
///
/// The third type parameter `E` is a **typestate** marker: it starts as [`NoEvict`] and
/// transitions to [`HasEvict`] after `.on_evict(…)` is called. This encodes at compile time
/// whether an eviction callback has been registered, allowing the two `build()` / `copy_from()`
/// overloads to impose `K: 'static + V: 'static` bounds only when `on_evict` is set. You will
/// see this parameter in IDE completions and compiler errors once you call `.on_evict(…)`;
/// it is otherwise invisible. The hasher `H` is last, matching
/// [`LruTtlCacheBuilder`](crate::LruTtlCacheBuilder)`<K, V, E, S>`.
pub struct ShardedLruTtlCacheBuilder<K, V, E = NoEvict, H = DefaultShardHasher> {
    shards: Option<usize>,
    max_size: Option<usize>,
    per_shard_max_size: Option<usize>,
    ttl: Option<Duration>,
    refresh: bool,
    hasher: Option<H>,
    on_evict: Option<OnEvict<K, V>>,
    _evict: PhantomData<E>,
}

impl<K, V> Default for ShardedLruTtlCacheBuilder<K, V> {
    fn default() -> Self {
        Self {
            shards: None,
            max_size: None,
            per_shard_max_size: None,
            ttl: None,
            refresh: false,
            hasher: Some(DefaultShardHasher::default()),
            on_evict: None,
            _evict: PhantomData,
        }
    }
}

impl<K, V, E, H> ShardedLruTtlCacheBuilder<K, V, E, H> {
    /// Set the requested total capacity (divided across shards via `div_ceil`).
    ///
    /// Eviction is enforced independently per shard. Each shard gets
    /// `ceil(size / shards)` entries, with a minimum of 16 per shard when
    /// `shards > 1`. This protects against premature evictions due to hash
    /// collisions in extremely small caches; if you require smaller, strict
    /// limits, configure `shards = 1`.
    ///
    /// # Minimum capacity
    ///
    /// Because each shard reserves a minimum of **16** entries when `shards > 1`, the effective
    /// total capacity is at least `shards * 16` and may **exceed** the requested `max_size` for
    /// small values (e.g. `max_size = 10` with 8 shards yields an effective capacity of 128).
    /// [`metrics()`](ShardedLruTtlCacheBase::metrics)'s `capacity` and `entry_count` reflect the
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
    pub fn hasher<H2: ShardHasher<K>>(self, hasher: H2) -> ShardedLruTtlCacheBuilder<K, V, E, H2> {
        ShardedLruTtlCacheBuilder {
            shards: self.shards,
            max_size: self.max_size,
            per_shard_max_size: self.per_shard_max_size,
            ttl: self.ttl,
            refresh: self.refresh,
            hasher: Some(hasher),
            on_evict: self.on_evict,
            _evict: PhantomData,
        }
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

    fn validated_parts(&self) -> Result<(Duration, usize, usize, usize), BuildError> {
        let ttl = self.ttl.ok_or(BuildError::MissingRequired("ttl"))?;
        crate::stores::validate_ttl(ttl)?;
        let n = checked_shard_count(self.shards)?;
        let mask = n - 1;
        let per_shard_cap = self.resolve_per_shard_cap(n)?;
        let total_cap = self.total_capacity(n, per_shard_cap)?;
        Ok((ttl, mask, per_shard_cap, total_cap))
    }
}

impl<K, V, H> ShardedLruTtlCacheBuilder<K, V, NoEvict, H> {
    /// Set a callback invoked when an entry is evicted by LRU capacity pressure,
    /// TTL-expiry sweeps via [`evict`](ShardedLruTtlCacheBase::evict), explicit
    /// [`cache_remove`](ConcurrentCached::cache_remove), or
    /// [`cache_remove_entry`](ConcurrentCached::cache_remove_entry).
    /// Does **not** fire on [`clear`](ShardedLruTtlCacheBase::clear);
    /// use [`cache_clear_with_on_evict`](ShardedLruTtlCacheBase::cache_clear_with_on_evict) to opt in.
    ///
    /// Capacity-eviction callbacks run while the affected shard's write lock is held. Do not call
    /// methods on the same sharded cache from the callback; doing so can deadlock if the callback
    /// re-enters the locked shard. TTL expiry sweeps via
    /// [`evict`](ShardedLruTtlCacheBase::evict) and explicit removes via
    /// [`cache_remove`](ConcurrentCached::cache_remove) /
    /// [`cache_remove_entry`](ConcurrentCached::cache_remove_entry) fire `on_evict` after
    /// releasing the shard lock and do not have this restriction.
    ///
    /// # Lifetime Bounds
    ///
    /// Setting this callback introduces `'static` bounds on `K` and `V` due to the need
    /// to map the callback across the internal store layers. If your keys/values have lifetimes,
    /// do not set an `on_evict` callback, or ensure they are `'static`.
    #[must_use]
    pub fn on_evict(
        self,
        on_evict: impl Fn(&K, &V) + Send + Sync + 'static,
    ) -> ShardedLruTtlCacheBuilder<K, V, HasEvict, H> {
        ShardedLruTtlCacheBuilder {
            shards: self.shards,
            max_size: self.max_size,
            per_shard_max_size: self.per_shard_max_size,
            ttl: self.ttl,
            refresh: self.refresh,
            hasher: self.hasher,
            on_evict: Some(Arc::new(on_evict)),
            _evict: PhantomData,
        }
    }

    /// Build the cache, returning an error if required fields are missing or invalid.
    ///
    /// Use [`ShardedLruTtlCache::builder()`] (or [`ShardedLruTtlCacheBase::builder()`]) to obtain
    /// a builder, set at least [`max_size`](Self::max_size) and [`ttl`](Self::ttl), then call
    /// `.build()`.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError`] if `size` (or `per_shard_max_size`) or `ttl` was not set, is `0`,
    /// or if both `max_size` and `per_shard_max_size` are set simultaneously. May also return
    /// [`BuildError::InvalidValue`] if the effective sharded capacity overflows `usize` or a
    /// per-shard allocation fails.
    #[must_use = "the Result from build() must be used"]
    pub fn build(self) -> Result<ShardedLruTtlCacheBase<K, V, H>, BuildError>
    where
        K: Hash + Eq + Clone,
        H: ShardHasher<K>,
    {
        let (ttl, mask, per_shard_cap, total_cap) = self.validated_parts()?;
        let n = mask + 1;

        let shards = (0..n)
            .map(|_| {
                let mut lru: LruCache<K, TimedEntry<V>> =
                    LruCache::builder().max_size(per_shard_cap).build()?;
                lru.disable_hit_miss_tracking();
                Ok(CachePadded(Shard::new(lru)))
            })
            .collect::<Result<Vec<_>, BuildError>>()?
            .into_boxed_slice();

        Ok(ShardedLruTtlCacheBase {
            inner: Arc::new(LruTtlInner {
                shards,
                shard_mask: mask,
                hasher: self
                    .hasher
                    .expect("hasher is always initialized via Default or .hasher()"),
                on_evict: None,
                ttl_nanos: AtomicU64::new(encode_ttl(ttl)),
                refresh: AtomicBool::new(self.refresh),
                non_capacity_evictions: AtomicU64::new(0),
                total_capacity: total_cap,
            }),
        })
    }

    /// Build the new cache and copy every non-expired entry from `existing` into it,
    /// preserving per-shard LRU ordering and original `TimedEntry` timestamps.
    /// Global recency rank is not guaranteed across shards after resharding.
    ///
    /// The target cache uses this builder's TTL setting when checking copied entries.
    /// For the same wall-clock expiry schedule, build the target with the same TTL as
    /// `existing`; a shorter or longer target TTL can make copied entries expire earlier
    /// or later than they would have in the source cache.
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
    /// configuration is invalid (the same conditions as [`build`](Self::build)):
    /// `size` (or `per_shard_max_size`) or `ttl` was not set or is `0`.
    #[must_use = "the Result from copy_from() must be used"]
    pub fn copy_from<H2: ShardHasher<K>>(
        self,
        existing: &ShardedLruTtlCacheBase<K, V, H2>,
    ) -> Result<ShardedLruTtlCacheBase<K, V, H>, BuildError>
    where
        K: Clone + Hash + Eq,
        V: Clone,
        H: ShardHasher<K>,
    {
        Ok(copy_from_lru_ttl(self.build()?, existing))
    }
}

impl<K, V, H> ShardedLruTtlCacheBuilder<K, V, HasEvict, H> {
    /// Build the cache, returning an error if required fields are missing or invalid.
    ///
    /// Use [`ShardedLruTtlCache::builder()`] (or [`ShardedLruTtlCacheBase::builder()`]) to obtain
    /// a builder, set at least [`max_size`](Self::max_size) and [`ttl`](Self::ttl), then call
    /// `.build()`.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError`] if `size` (or `per_shard_max_size`) or `ttl` was not set, is `0`,
    /// or if both `max_size` and `per_shard_max_size` are set simultaneously. May also return
    /// [`BuildError::InvalidValue`] if the effective sharded capacity overflows `usize` or a
    /// per-shard allocation fails.
    #[must_use = "the Result from build() must be used"]
    pub fn build(self) -> Result<ShardedLruTtlCacheBase<K, V, H>, BuildError>
    where
        K: Hash + Eq + Clone + 'static,
        V: 'static,
        H: ShardHasher<K>,
    {
        let (ttl, mask, per_shard_cap, total_cap) = self.validated_parts()?;
        let n = mask + 1;

        #[allow(clippy::type_complexity)]
        let lru_on_evict: Option<Arc<dyn Fn(&K, &TimedEntry<V>) + Send + Sync>> =
            self.on_evict.as_ref().map(|cb| {
                let cb = Arc::clone(cb);
                let f: Arc<dyn Fn(&K, &TimedEntry<V>) + Send + Sync> =
                    Arc::new(move |k: &K, entry: &TimedEntry<V>| cb(k, &entry.value));
                f
            });

        let shards = (0..n)
            .map(|_| {
                let mut lru: LruCache<K, TimedEntry<V>> =
                    LruCache::builder().max_size(per_shard_cap).build()?;
                lru.on_evict = lru_on_evict.clone();
                lru.disable_hit_miss_tracking();
                Ok(CachePadded(Shard::new(lru)))
            })
            .collect::<Result<Vec<_>, BuildError>>()?
            .into_boxed_slice();

        Ok(ShardedLruTtlCacheBase {
            inner: Arc::new(LruTtlInner {
                shards,
                shard_mask: mask,
                hasher: self
                    .hasher
                    .expect("hasher is always initialized via Default or .hasher()"),
                on_evict: self.on_evict,
                ttl_nanos: AtomicU64::new(encode_ttl(ttl)),
                refresh: AtomicBool::new(self.refresh),
                non_capacity_evictions: AtomicU64::new(0),
                total_capacity: total_cap,
            }),
        })
    }

    /// Build the new cache and copy every non-expired entry from `existing` into it,
    /// preserving per-shard LRU ordering and original `TimedEntry` timestamps.
    /// Global recency rank is not guaranteed across shards after resharding.
    ///
    /// The target cache uses this builder's TTL setting when checking copied entries.
    /// For the same wall-clock expiry schedule, build the target with the same TTL as
    /// `existing`; a shorter or longer target TTL can make copied entries expire earlier
    /// or later than they would have in the source cache.
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
    /// configuration is invalid (the same conditions as [`build`](Self::build)):
    /// `size` (or `per_shard_max_size`) or `ttl` was not set or is `0`.
    #[must_use = "the Result from copy_from() must be used"]
    pub fn copy_from<H2: ShardHasher<K>>(
        self,
        existing: &ShardedLruTtlCacheBase<K, V, H2>,
    ) -> Result<ShardedLruTtlCacheBase<K, V, H>, BuildError>
    where
        K: Clone + Hash + Eq + 'static,
        V: Clone + 'static,
        H: ShardHasher<K>,
    {
        Ok(copy_from_lru_ttl(self.build()?, existing))
    }
}

fn copy_from_lru_ttl<K, V, H, H2>(
    new_cache: ShardedLruTtlCacheBase<K, V, H>,
    existing: &ShardedLruTtlCacheBase<K, V, H2>,
) -> ShardedLruTtlCacheBase<K, V, H>
where
    K: Clone + Hash + Eq,
    V: Clone,
    H: ShardHasher<K>,
    H2: ShardHasher<K>,
{
    let now = Instant::now();
    for shard in existing.inner.shards.iter() {
        let entries: Vec<(K, TimedEntry<V>)> = {
            let guard = shard.lock.read();
            guard.iter_order()
        };
        for (k, entry) in entries.into_iter().rev() {
            // Skip entries already expired per their per-entry expires_at.
            if entry.expires_at.is_some_and(|t| now >= t) {
                continue;
            }
            let new_shard = new_cache.shard_of(&k);
            new_shard.lock.write().cache_set(k, entry);
        }
    }
    new_cache
}

impl<K, V, H> ConcurrentCloneCached<K, V> for ShardedLruTtlCacheBase<K, V, H>
where
    K: Hash + Eq + Clone,
    V: Clone,
    H: ShardHasher<K>,
{
    /// Returns `(Some(v), false)` for a live entry (hit, LRU promoted), `(Some(v), true)` for an
    /// expired entry (miss, **no removal**, no LRU promotion, no eviction counter), or
    /// `(None, false)` when absent (miss).
    fn cache_get_with_expiry_status(&self, k: &K) -> (Option<V>, bool) {
        let shard = self.shard_of(k);
        let refresh = self.inner.refresh.load(Ordering::Relaxed);
        let mut guard = shard.lock.write();
        // Common case (live hit) in a single lookup: `get_if`/`get_mut_if` promote LRU
        // recency only when the predicate reports the entry live, and leave it in place
        // (no removal, no promotion) when it reports expired. The rarer expired/absent
        // case then takes one extra peek to recover the stale value without removing it.
        let live = if refresh {
            guard
                .get_mut_if(k, |e| e.expires_at.is_none_or(|t| Instant::now() < t))
                .map(|e| {
                    let now = Instant::now();
                    e.expires_at = self.compute_expires_at(now).or(e.expires_at);
                    e.value.clone()
                })
        } else {
            guard
                .get_if(k, |e| e.expires_at.is_none_or(|t| Instant::now() < t))
                .map(|e| e.value.clone())
        };
        if let Some(value) = live {
            drop(guard);
            shard.hits.fetch_add(1, Ordering::Relaxed);
            return (Some(value), false);
        }
        // Not a live hit: either expired (still present, left in place) or absent.
        // A single peek distinguishes them and clones the stale value without removal.
        let stale = guard.cache_peek(k).map(|e| e.value.clone());
        drop(guard);
        shard.misses.fetch_add(1, Ordering::Relaxed);
        match stale {
            Some(v) => (Some(v), true),
            None => (None, false),
        }
    }

    /// Non-renewing read: takes only a read lock, does not promote LRU recency, does not update
    /// the TTL timestamp, does not touch the hits/misses counters, and does not remove the entry.
    /// Returns `(Some(v), expired)` for a present entry (expired or not) or `(None, false)` when
    /// absent.
    fn cache_peek_with_expiry_status(&self, k: &K) -> (Option<V>, bool) {
        let shard = self.shard_of(k);
        let guard = shard.lock.read();
        match guard.cache_peek(k) {
            None => (None, false),
            Some(entry) => {
                let expired = entry.expires_at.is_some_and(|t| Instant::now() >= t);
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
    fn cache_set_over_expired_returns_none_fires_on_evict_and_counts() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU64, Ordering as AOrd};
        let count = Arc::new(AtomicU64::new(0));
        let count2 = count.clone();
        let c = ShardedLruTtlCacheBase::<u32, u32>::builder()
            .shards(1)
            .max_size(4)
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
    fn builder_generic_param_order_is_eviction_typestate_then_hasher() {
        // API-5: ShardedLruTtlCacheBuilder's params are <K, V, E, H> (eviction typestate
        // before hasher, hasher last), matching LruTtlCacheBuilder<K, V, E, S>. Naming them
        // positionally in that order must compile; this pins the order against reordering.
        let _default: ShardedLruTtlCacheBuilder<u32, u32, NoEvict, DefaultShardHasher> =
            ShardedLruTtlCache::<u32, u32>::builder();

        // A custom hasher slots into the last position, and .on_evict flips the typestate to
        // HasEvict (third position) while the hasher stays last.
        let cache = ShardedLruTtlCache::<u32, u32>::builder()
            .shards(1)
            .max_size(8)
            .ttl(Duration::from_secs(60))
            .hasher(DefaultShardHasher::default())
            .on_evict(|_, _| {})
            .build()
            .unwrap();
        let _typed: ShardedLruTtlCacheBase<u32, u32, DefaultShardHasher> = cache;
    }

    #[test]
    fn new_returns_ready_cache_respecting_max_size_and_ttl() {
        // shards(1) gives an exact eviction bound.
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .shards(1)
            .max_size(2)
            .ttl(Duration::from_millis(10))
            .build()
            .unwrap();
        assert_eq!(c.ttl(), Some(Duration::from_millis(10)));
        SyncConcurrentCached::cache_set(&c, 1, 10).unwrap();
        SyncConcurrentCached::cache_set(&c, 2, 20).unwrap();
        SyncConcurrentCached::cache_set(&c, 3, 30).unwrap(); // evicts LRU (1)
        assert_eq!(c.len(), 2);
        assert_eq!(SyncConcurrentCached::cache_get(&c, &1).unwrap(), None);
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert_eq!(
            SyncConcurrentCached::cache_get(&c, &2).unwrap(),
            None,
            "entry must expire after ttl"
        );

        // Inherent `new` returns a ready cache too.
        let c2 = ShardedLruTtlCache::<u32, u32>::new(64, Duration::from_secs(60));
        assert_eq!(SyncConcurrentCached::cache_set(&c2, 1, 100).unwrap(), None);
        assert_eq!(SyncConcurrentCached::cache_get(&c2, &1).unwrap(), Some(100));

        // `new(N, ttl)` must forward N to the builder — capacity must equal the builder path.
        let ttl = Duration::from_secs(60);
        assert_eq!(
            ShardedLruTtlCache::<u32, u32>::new(1024, ttl).capacity(),
            ShardedLruTtlCache::<u32, u32>::builder()
                .max_size(1024)
                .ttl(ttl)
                .build()
                .unwrap()
                .capacity()
        );
    }

    #[test]
    #[should_panic(expected = "non-zero max_size and non-zero ttl")]
    fn new_zero_max_size_panics() {
        let _c = ShardedLruTtlCache::<u32, u32>::new(0, Duration::from_secs(1));
    }

    #[test]
    #[should_panic(expected = "non-zero max_size and non-zero ttl")]
    fn new_zero_ttl_panics() {
        let _c = ShardedLruTtlCache::<u32, u32>::new(2, Duration::ZERO);
    }

    #[test]
    fn ttl_secs_and_ttl_millis_set_duration() {
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(64)
            .ttl_secs(7)
            .build()
            .unwrap();
        assert_eq!(c.ttl(), Some(Duration::from_secs(7)));

        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(64)
            .ttl_millis(250)
            .build()
            .unwrap();
        assert_eq!(c.ttl(), Some(Duration::from_millis(250)));
    }

    #[test]
    fn ttl_setters_override_last_writer_wins() {
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(64)
            .ttl(Duration::from_secs(10))
            .ttl_secs(5)
            .build()
            .unwrap();
        assert_eq!(c.ttl(), Some(Duration::from_secs(5)));

        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(64)
            .ttl_secs(10)
            .ttl_millis(500)
            .build()
            .unwrap();
        assert_eq!(c.ttl(), Some(Duration::from_millis(500)));
    }

    #[test]
    fn basic_get_set_remove() {
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(64)
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
    fn cache_remove_fires_on_evict_and_increments_metrics() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let count = Arc::new(AtomicUsize::new(0));
        let count2 = count.clone();
        let c = ShardedLruTtlCacheBase::<u32, u32>::builder()
            .max_size(64)
            .ttl(Duration::from_secs(60))
            .shards(1)
            .on_evict(move |_, _| {
                count2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();

        SyncConcurrentCached::cache_set(&c, 1, 10).expect("insert must succeed");
        let before = c
            .metrics()
            .evictions
            .expect("eviction-tracking stores report an evictions count");
        assert_eq!(
            SyncConcurrentCached::cache_remove(&c, &1).expect("key must be present"),
            Some(10)
        );
        assert_eq!(
            SyncConcurrentCached::cache_remove(&c, &999).expect("cache_remove must succeed"),
            None
        );
        let after = c
            .metrics()
            .evictions
            .expect("eviction-tracking stores report an evictions count");

        assert_eq!(count.load(Ordering::Relaxed), 1);
        assert_eq!(after - before, 1);
    }

    #[test]
    fn clone_shares_state() {
        let c1 = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(64)
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
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(64)
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
    fn lru_eviction_fires() {
        use std::sync::atomic::{AtomicUsize, Ordering as AO};
        let count = std::sync::Arc::new(AtomicUsize::new(0));
        let count2 = count.clone();
        let c = ShardedLruTtlCacheBase::<u32, u32>::builder()
            .max_size(8)
            .shards(1)
            .ttl(Duration::from_secs(60))
            .on_evict(move |_, _| {
                count2.fetch_add(1, AO::Relaxed);
            })
            .build()
            .unwrap();
        for i in 0..16u32 {
            SyncConcurrentCached::cache_set(&c, i, i).expect("insert must succeed");
        }
        assert!(
            count.load(AO::Relaxed) > 0,
            "LRU eviction should have fired"
        );
    }

    #[test]
    fn per_shard_max_size_and_size_exclusive() {
        let err = ShardedLruTtlCacheBase::<u32, u32>::builder()
            .max_size(100)
            .per_shard_max_size(10)
            .ttl(Duration::from_secs(60))
            .build();
        assert!(err.is_err());
    }

    #[test]
    fn build_rejects_overflowing_shards_and_capacity() {
        let err = ShardedLruTtlCacheBase::<u32, u32>::builder()
            .max_size(1)
            .ttl(Duration::from_secs(60))
            .shards(usize::MAX)
            .build();
        assert!(matches!(
            err,
            Err(BuildError::InvalidValue {
                field: "shards",
                ..
            })
        ));

        let err = ShardedLruTtlCacheBase::<u32, u32>::builder()
            .per_shard_max_size(usize::MAX)
            .ttl(Duration::from_secs(60))
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
    fn builder_without_on_evict_does_not_require_static_keys_or_values() {
        let key = String::from("key");
        let value = String::from("value");
        let cache: ShardedLruTtlCacheBase<&str, &str> = ShardedLruTtlCache::builder()
            .max_size(8)
            .ttl(Duration::from_secs(60))
            .build()
            .expect("valid builder config");

        SyncConcurrentCached::cache_set(&cache, key.as_str(), value.as_str())
            .expect("insert must succeed");
        assert_eq!(
            SyncConcurrentCached::cache_get(&cache, &key.as_str()).expect("key was just inserted"),
            Some(value.as_str())
        );
    }

    #[test]
    fn set_ttl_inherent() {
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(64)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        let prev = c.set_ttl(Duration::from_secs(30));
        assert_eq!(prev, Some(Duration::from_secs(60)));
        assert_eq!(c.ttl(), Some(Duration::from_secs(30)));
    }

    #[test]
    fn copy_from_skips_expired() {
        let old = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(64)
            .ttl(Duration::from_millis(50))
            .build()
            .unwrap();
        for i in 0..10u32 {
            SyncConcurrentCached::cache_set(&old, i, i).expect("insert must succeed");
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        let new_cache = ShardedLruTtlCacheBase::<u32, u32>::builder()
            .max_size(64)
            .ttl(Duration::from_secs(60))
            .copy_from(&old)
            .unwrap();
        assert_eq!(new_cache.len(), 0);
    }

    #[test]
    fn copy_from_preserves_live_entries() {
        // Use shards(1) to avoid per-shard capacity eviction during insertion.
        let old = ShardedLruTtlCacheBase::<u32, u32>::builder()
            .max_size(1024)
            .shards(1)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        for i in 0..20u32 {
            SyncConcurrentCached::cache_set(&old, i, i * 10).expect("insert must succeed");
        }
        let new_cache = ShardedLruTtlCacheBase::<u32, u32>::builder()
            .max_size(1024)
            .shards(4)
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
    fn copy_from_respects_capacity() {
        let old = ShardedLruTtlCacheBase::<u32, u32>::builder()
            .max_size(64)
            .shards(1)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        for i in 0..32u32 {
            SyncConcurrentCached::cache_set(&old, i, i).expect("insert must succeed");
        }
        let new_cache = ShardedLruTtlCacheBase::<u32, u32>::builder()
            .max_size(16)
            .shards(1)
            .ttl(Duration::from_secs(60))
            .copy_from(&old)
            .unwrap();
        assert!(new_cache.len() <= 16);
    }

    #[test]
    fn build_reports_invalid_config() {
        let err = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(0)
            .ttl(Duration::from_secs(60))
            .build();
        assert!(matches!(
            err,
            Err(BuildError::InvalidValue {
                field: "max_size",
                ..
            })
        ));

        let err = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(1)
            .ttl(Duration::from_secs(60))
            .shards(0)
            .build();
        assert!(matches!(
            err,
            Err(BuildError::InvalidValue {
                field: "shards",
                ..
            })
        ));

        let err = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(1)
            .ttl(Duration::from_nanos(0))
            .build();
        assert!(matches!(
            err,
            Err(BuildError::InvalidValue { field: "ttl", .. })
        ));
    }

    #[test]
    fn send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ShardedLruTtlCache<u32, u32>>();
    }

    #[test]
    fn build_rejects_zero_ttl() {
        let err = ShardedLruTtlCacheBase::<u32, u32>::builder()
            .max_size(8)
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
        let c = ShardedLruTtlCacheBase::<u32, u32>::builder()
            .shards(1)
            .max_size(64)
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
        let c = ShardedLruTtlCacheBase::<u32, u32>::builder()
            .shards(1)
            .max_size(64)
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
        let c = ShardedLruTtlCacheBase::<u32, u32>::builder()
            .max_size(64)
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
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(64)
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
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(64)
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
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(64)
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
        let c = ShardedLruTtlCacheBase::<u32, u32>::builder()
            .max_size(64)
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
        let c = ShardedLruTtlCacheBase::<u32, u32>::builder()
            .max_size(64)
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
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(64)
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
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(64)
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
        let c = ShardedLruTtlCacheBase::<u32, u32>::builder()
            .max_size(64)
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

        // Entry must NOT have been removed — a second expiry-status call still sees it.
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
    fn concurrent_clone_cached_live_lookup_promotes_lru() {
        // shards(1) + max_size(2): a single shard with a 2-entry LRU bound, so eviction
        // order is deterministic and observable.
        let c = ShardedLruTtlCacheBase::<u32, u32>::builder()
            .max_size(2)
            .ttl(Duration::from_secs(60))
            .shards(1)
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1u32, 10u32).expect("insert must succeed");
        SyncConcurrentCached::cache_set(&c, 2u32, 20u32).expect("insert must succeed");

        // A live expiry-status lookup of key 1 must promote it to most-recently-used,
        // so the next insertion evicts key 2 (now least-recently-used), not key 1.
        assert_eq!(
            ConcurrentCloneCached::cache_get_with_expiry_status(&c, &1u32),
            (Some(10), false),
            "live lookup must return the value"
        );

        SyncConcurrentCached::cache_set(&c, 3u32, 30u32).expect("insert must succeed");

        assert_eq!(
            SyncConcurrentCached::cache_get(&c, &1u32).expect("cache_get must succeed"),
            Some(10),
            "key 1 must survive eviction because the live expiry-status lookup promoted it"
        );
        assert_eq!(
            SyncConcurrentCached::cache_get(&c, &2u32).expect("cache_get must succeed"),
            None,
            "key 2 must be evicted as the least-recently-used entry"
        );
    }

    #[test]
    fn peek_with_expiry_status_no_side_effects() {
        // shards(1) makes counter captures exact (no cross-shard aggregation noise).
        let c = ShardedLruTtlCacheBase::<u32, u32>::builder()
            .max_size(64)
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
    fn peek_with_expiry_status_does_not_promote_lru() {
        // max_size(2) + shards(1): with only 2 slots, inserting a third entry
        // evicts the LRU entry. If peek promoted recency, it would change which
        // entry survives; if it does not promote, the pre-peek LRU order holds.
        let c = ShardedLruTtlCacheBase::<u32, u32>::builder()
            .max_size(2)
            .ttl(Duration::from_secs(60))
            .shards(1)
            .build()
            .unwrap();

        // Insert order: key 1, then key 2.  LRU is key 1 (oldest access).
        SyncConcurrentCached::cache_set(&c, 1u32, 10u32).expect("insert must succeed");
        SyncConcurrentCached::cache_set(&c, 2u32, 20u32).expect("insert must succeed");

        // Peek key 1 — must NOT promote it to MRU.
        let (val, expired) = ConcurrentCloneCached::cache_peek_with_expiry_status(&c, &1u32);
        assert_eq!(val, Some(10), "peek must return the value");
        assert!(!expired, "peek must report expired=false for a live entry");

        // Counters unchanged: no hits, no misses.
        let m = c.metrics();
        assert_eq!(m.hits, Some(0), "peek must not increment hits");
        assert_eq!(m.misses, Some(0), "peek must not increment misses");

        // Inserting key 3 must evict key 1 (still LRU because peek did not
        // promote it), not key 2.
        SyncConcurrentCached::cache_set(&c, 3u32, 30u32).expect("insert must succeed");

        // key 1 evicted (LRU), key 2 and key 3 survive.
        assert!(
            SyncConcurrentCached::cache_get(&c, &1u32)
                .expect("cache_get must succeed")
                .is_none(),
            "key 1 must be evicted as LRU (peek must not have promoted it)"
        );
        assert_eq!(
            SyncConcurrentCached::cache_get(&c, &2u32).expect("cache_get must succeed"),
            Some(20),
            "key 2 must survive"
        );
        assert_eq!(
            SyncConcurrentCached::cache_get(&c, &3u32).expect("cache_get must succeed"),
            Some(30),
            "key 3 must survive"
        );
    }

    #[test]
    fn peek_with_expiry_status_stale_entry_no_side_effects() {
        let c = ShardedLruTtlCacheBase::<u32, u32>::builder()
            .max_size(64)
            .ttl(Duration::from_millis(50))
            .shards(1)
            .build()
            .unwrap();

        SyncConcurrentCached::cache_set(&c, 1u32, 77u32).expect("insert must succeed");
        std::thread::sleep(std::time::Duration::from_millis(100));

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
        let c = ShardedLruTtlCacheBase::<u32, u32>::builder()
            .refresh_on_hit(true)
            .max_size(64)
            .ttl(Duration::from_millis(50))
            .shards(1)
            .build()
            .unwrap();

        SyncConcurrentCached::cache_set(&c, 1u32, 42u32).expect("insert must succeed");

        // Entry is live; peek must return the value and report not-expired.
        let (val, expired) = ConcurrentCloneCached::cache_peek_with_expiry_status(&c, &1u32);
        assert_eq!(val, Some(42), "live peek must return the value");
        assert!(!expired, "live peek must report expired=false");

        // Wait past the original TTL.
        std::thread::sleep(std::time::Duration::from_millis(100));

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
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(64)
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
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(64)
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
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(64)
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
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(64)
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
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(64)
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
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(64)
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
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(64)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        use_trait(&c, 1, 100);
    }
}
