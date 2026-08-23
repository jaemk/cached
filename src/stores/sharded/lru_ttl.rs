use std::hash::Hash;
use std::marker::PhantomData;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

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
    CachePadded, DefaultShardHasher, Shard, ShardHasher, checked_per_shard_cap_from_total,
    checked_shard_count, decode_ttl, default_shard_count_for_capacity, encode_ttl,
    per_shard_cap_from_total, shard_index,
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
    /// Total logical capacity (sum of per-shard caps). Stored as `AtomicUsize` so
    /// [`set_max_size`](ShardedLruTtlCache::set_max_size) can update it from `&self`.
    total_capacity: AtomicUsize,
}

/// A fully-concurrent, partitioned, LRU-bounded, TTL-expiring in-memory cache.
///
/// Wraps an `Arc` — `clone()` is an Arc-share (shared state), not a deep copy.
/// Use [`deep_clone`](ShardedLruTtlCache::deep_clone) to get an independent copy.
///
/// **Note**: `K` and `V` must implement `Clone` (`K` for LRU key tracking; `V` because reads
/// return owned values cloned from under the shard lock).
///
/// The runtime TTL controls (`ttl` / `set_ttl` / `try_set_ttl` / `unset_ttl`) live on
/// [`ConcurrentCacheTtl`](crate::ConcurrentCacheTtl), and the refresh-on-hit controls
/// (`refresh_on_hit` / `set_refresh_on_hit`) on
/// [`ConcurrentCacheRefreshOnHit`](crate::ConcurrentCacheRefreshOnHit); import them (or
/// `cached::prelude::*`) to call them. Builder setters are unaffected.
///
/// The shard-selection hasher `H` defaults to [`DefaultShardHasher`] (ahash-backed when the
/// `ahash` feature is enabled, otherwise `std::collections::hash_map::RandomState`), so
/// `ShardedLruTtlCache<K, V>` names the common case. To use a custom [`ShardHasher`], call
/// [`ShardedLruTtlCache::builder()`] and then [`hasher`](ShardedLruTtlCacheBuilder::hasher),
/// which switches `H` to your hasher.
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
pub struct ShardedLruTtlCache<K, V, H = DefaultShardHasher> {
    inner: Arc<LruTtlInner<K, V, H>>,
}

impl<K, V, H> Clone for ShardedLruTtlCache<K, V, H> {
    /// Arc-share clone — both handles point to the same underlying cache.
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<K, V, H> ShardedLruTtlCache<K, V, H> {
    /// Resolve the currently configured TTL, independent of hasher bounds.
    ///
    /// Returns `None` when expiry is disabled (entries never expire), otherwise
    /// `Some(ttl)`.
    #[inline]
    fn ttl_duration_impl(&self) -> Option<Duration> {
        decode_ttl(self.inner.ttl_nanos.load(Ordering::Relaxed))
    }

    /// Sum of the per-shard counters for evictions **not** driven by LRU capacity pressure:
    /// TTL expiry (lazily on [`cache_get`](ConcurrentCached::cache_get) or in bulk via
    /// [`evict`](ShardedLruTtlCache::evict)), explicit removes
    /// ([`cache_remove`](ConcurrentCached::cache_remove) /
    /// [`cache_remove_entry`](ConcurrentCached::cache_remove_entry)),
    /// [`retain`](ShardedLruTtlCache::retain), and
    /// [`cache_clear_with_on_evict`](ShardedLruTtlCache::cache_clear_with_on_evict).
    ///
    /// These live in [`Shard::evictions`], one atomic per shard (like `hits`/`misses`), rather
    /// than in a single process-wide counter on `Arc<Inner>`: a thread bumping it has just held
    /// that shard's lock, so the line is already owned exclusively and no cross-core traffic is
    /// added. LRU **capacity** evictions remain in each shard's inner `LruCache::evictions`;
    /// [`metrics`](ShardedLruTtlCache::metrics) sums the two families.
    fn non_capacity_evictions(&self) -> u64 {
        self.inner
            .shards
            .iter()
            .map(|s| s.evictions.load(Ordering::Relaxed))
            .sum()
    }
}

impl<K, V, H> std::fmt::Debug for ShardedLruTtlCache<K, V, H> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ttl = self.ttl_duration_impl();
        f.debug_struct("ShardedLruTtlCache")
            .field("shards", &self.inner.shards.len())
            .field(
                "capacity",
                &self.inner.total_capacity.load(Ordering::Relaxed),
            )
            .field("ttl", &ttl)
            .finish_non_exhaustive()
    }
}

impl<K, V> ShardedLruTtlCache<K, V, DefaultShardHasher>
where
    K: Hash + Eq + Clone,
{
    /// Construct a ready-to-use [`ShardedLruTtlCache`] holding up to roughly `max_size`
    /// entries total with the given `ttl`, the [`DefaultShardHasher`], and a default shard
    /// count.
    ///
    /// Note that the effective total capacity can still exceed `max_size` for small values
    /// because each shard reserves a minimum capacity (see
    /// [`max_size`](ShardedLruTtlCacheBuilder::max_size)). The default shard count is now scaled
    /// down for small `max_size` (roughly `max_size / 16` shards, capped by the CPU-derived
    /// default), so the overshoot is modest -- `max_size = 100` yields 8 shards x 16 = 128
    /// effective capacity. For a custom hasher, shard count, per-shard cap, `refresh_on_hit`, or
    /// `on_evict`, use [`builder`](Self::builder).
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
    /// builder's hasher type and `build` then yields a `ShardedLruTtlCache<K, V, H>` over that
    /// hasher. `new` and `builder` exist only on the default-hasher instantiation
    /// `ShardedLruTtlCache<K, V, DefaultShardHasher>`, so a custom hasher is always introduced
    /// via `hasher`, never a `ShardedLruTtlCache::<_, _, H>` turbofish.
    #[must_use]
    pub fn builder() -> ShardedLruTtlCacheBuilder<K, V, DefaultShardHasher> {
        ShardedLruTtlCacheBuilder::default()
    }
}

impl<K, V, H> ShardedLruTtlCache<K, V, H>
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

impl<K: Clone + Hash + Eq, V: Clone, H: ShardHasher<K>> ShardedLruTtlCache<K, V, H> {
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
                // Carry the shard's non-capacity eviction count across too (it used to live
                // in a single process-wide counter that `deep_clone` copied wholesale), so
                // the clone's `metrics().evictions` matches the source's.
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
            inner: Arc::new(LruTtlInner {
                shards,
                shard_mask: self.inner.shard_mask,
                hasher: self.inner.hasher.clone(),
                on_evict: self.inner.on_evict.clone(),
                ttl_nanos: AtomicU64::new(self.inner.ttl_nanos.load(Ordering::Relaxed)),
                refresh: AtomicBool::new(self.inner.refresh.load(Ordering::Relaxed)),
                total_capacity: AtomicUsize::new(self.inner.total_capacity.load(Ordering::Relaxed)),
            }),
        }
    }
}

impl<K, V, H: ShardHasher<K>> ShardedLruTtlCache<K, V, H>
where
    K: Hash + Eq + Clone,
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
    /// observable side effects: no LRU recency update, no TTL refresh, no hit/miss
    /// metrics, no lazy removal of an expired entry. The single-owner counterpart is
    /// [`CachedPeek::cache_peek`](crate::CachedPeek::cache_peek); the sharded stores
    /// return a clone rather than a reference because the value lives behind a
    /// per-shard lock.
    #[must_use]
    pub fn peek(&self, k: &K) -> Option<V> {
        use crate::CachedPeek;
        let shard = self.shard_of(k);
        let guard = shard.lock.read();
        guard
            .cache_peek(k)
            .filter(|entry| entry.expires_at.is_none_or(|t| Instant::now() < t))
            .map(|entry| entry.value.clone())
    }
}

impl<K, V, H: ShardHasher<K>> ShardedLruTtlCache<K, V, H>
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
        let mut non_capacity_evictions = 0u64;
        let mut size = 0usize;
        for shard in self.inner.shards.iter() {
            hits += shard.hits.load(Ordering::Relaxed);
            misses += shard.misses.load(Ordering::Relaxed);
            // Per-shard non-capacity evictions (expiry / removes / retain / clear); the
            // inner `LruCache` counter below holds this shard's capacity evictions. The two
            // families are disjoint, so summing them cannot double-count.
            non_capacity_evictions += shard.evictions.load(Ordering::Relaxed);
            let guard = shard.lock.read();
            if let Some(e) = guard.cache_evictions() {
                lru_evictions += e;
            }
            size += guard.cache_size();
        }
        CacheMetrics {
            hits: Some(hits),
            misses: Some(misses),
            evictions: Some(lru_evictions + non_capacity_evictions),
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
                // `drain_all` walks each shard's LRU chain once taking owned pairs in
                // MRU -> LRU order -- the same order the old "clone every key, then
                // `pop_raw` each one" drain fired in, but with zero key clones and zero
                // re-hashing.
                guard.drain_all()
            };
            if !removed.is_empty() {
                shard
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

    /// Remove every entry that is TTL-expired **or** for which `keep` returns `false` — expired
    /// entries are removed regardless of `keep`, matching
    /// [`LruTtlCache::retain`](crate::LruTtlCache::retain) semantics. `on_evict` fires (if
    /// configured) and `metrics().evictions` increments once per removed entry (via the per-shard
    /// non-capacity eviction counter, the same one used by [`evict`](Self::evict)). The LRU recency
    /// order of the surviving entries in each shard is unchanged.
    ///
    /// Shards are processed one at a time under their own write lock, so this is **not atomic**
    /// across shards: a concurrent reader may observe some shards already filtered and others not
    /// yet touched. `keep` runs while the affected shard's write lock is held — do not call
    /// methods on this same cache from inside `keep`, as re-entering the locked shard can
    /// deadlock. `on_evict` fires after the shard's write lock has been released, once per removed
    /// entry, in shard order (and in each shard's iteration order for that shard's removals).
    /// Because callbacks run between shard sweeps, an `on_evict` that inserts into a shard this
    /// call has not yet visited will have that entry filtered by the same in-flight `retain`.
    ///
    /// Returns the total number of entries removed across all shards for this call, folding
    /// together predicate-rejected entries and entries swept for having already expired -- the
    /// two are not distinguished in the count. Not `#[must_use]`: discarding the count is a
    /// legitimate and common use.
    pub fn retain<F: FnMut(&K, &V) -> bool>(&self, mut keep: F) -> usize {
        let now = Instant::now();
        let mut total_removed = 0usize;
        for shard in self.inner.shards.iter() {
            let removed: Vec<(K, TimedEntry<V>)> = {
                let mut guard = shard.lock.write();
                let doomed: Vec<K> = guard
                    .iter()
                    .filter_map(|(k, entry)| {
                        let expired = entry.expires_at.is_some_and(|t| now >= t);
                        if expired || !keep(k, &entry.value) {
                            Some(k.clone())
                        } else {
                            None
                        }
                    })
                    .collect();
                let mut removed = Vec::with_capacity(doomed.len());
                for k in doomed {
                    if let Some(pair) = guard.pop_raw(&k) {
                        removed.push(pair);
                    }
                }
                removed
            };
            total_removed += removed.len();
            if !removed.is_empty() {
                shard
                    .evictions
                    .fetch_add(removed.len() as u64, Ordering::Relaxed);
                if let Some(on_evict) = &self.inner.on_evict {
                    for (k, entry) in &removed {
                        on_evict(k, &entry.value);
                    }
                }
            }
        }
        total_removed
    }

    /// Effective total capacity across all shards.
    ///
    /// When constructed with [`max_size`](ShardedLruTtlCacheBuilder::max_size), this may
    /// be larger than the requested size because per-shard capacity is rounded
    /// up with ceiling division.
    #[doc(alias = "size")]
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
    /// [`LruTtlCache::set_max_size`](crate::LruTtlCache::set_max_size) signature.
    ///
    /// Takes `&self`: shards use interior mutability (per-shard write locks), so
    /// the method is callable through `Arc` or any shared reference — no external
    /// lock is needed, unlike the `&mut self` single-owner counterpart.
    ///
    /// The new per-shard capacity is recomputed using the same policy the builder
    /// uses for [`max_size`](ShardedLruTtlCacheBuilder::max_size): ceiling division
    /// across shards with a minimum of 16 entries per shard when `shards > 1`.
    /// After resizing, any configuration previously set via
    /// [`per_shard_max_size`](ShardedLruTtlCacheBuilder::per_shard_max_size) is replaced
    /// by the total-based policy.
    ///
    /// On shrink, excess LRU entries are evicted per shard: `on_evict` fires for
    /// each evicted entry and the eviction counter (LRU capacity evictions) is
    /// incremented accordingly. The shrink evicts strictly by LRU recency and
    /// ignores TTL state — a TTL-expired but recently-used entry survives while
    /// a live but least-recently-used entry is evicted. Call
    /// [`evict`](Self::evict) first to sweep expired entries if they should be
    /// dropped preferentially.
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
    /// [`try_set_max_size`](ShardedLruTtlCache::try_set_max_size) to avoid either panic.
    ///
    /// # See also
    ///
    /// [`ShardedLruCache::set_max_size`](crate::ShardedLruCache::set_max_size) and
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

    /// Fallible counterpart of [`set_max_size`](ShardedLruTtlCache::set_max_size): validates
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
        total
    }
}

impl<K, V, H> ConcurrentCacheEvict for ShardedLruTtlCache<K, V, H>
where
    K: Hash + Eq + Clone,
    H: ShardHasher<K>,
{
    fn evict(&self) -> usize {
        ShardedLruTtlCache::evict(self)
    }
}

impl<K, V, H> ConcurrentCacheBase for ShardedLruTtlCache<K, V, H>
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
        let mut lru_evictions = 0u64;
        for shard in self.inner.shards.iter() {
            let guard = shard.lock.read();
            if let Some(e) = Cached::cache_evictions(&*guard) {
                lru_evictions += e;
            }
        }
        Some(lru_evictions + self.non_capacity_evictions())
    }
}

impl<K, V, H> ConcurrentCacheTtl for ShardedLruTtlCache<K, V, H>
where
    K: Hash + Eq + Clone,
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

impl<K, V, H> ConcurrentCacheRefreshOnHit for ShardedLruTtlCache<K, V, H>
where
    K: Hash + Eq + Clone,
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

impl<K, V, H> ConcurrentCached<K, V> for ShardedLruTtlCache<K, V, H>
where
    K: Hash + Eq + Clone,
    V: Clone,
    H: ShardHasher<K>,
{
    fn cache_get(&self, k: &K) -> Result<Option<V>, Self::Error> {
        let shard = self.shard_of(k);
        let refresh = self.inner.refresh.load(Ordering::Relaxed);
        // One clock sample per operation, taken before the lock: it decides expiry and (when
        // refreshing) seeds the new `expires_at`, so the critical section contains no
        // `Instant::now()` syscall at all.
        let now = Instant::now();

        let mut guard = shard.lock.write();

        // The common case (a live hit) resolves in a SINGLE hash + probe:
        // `get_if`/`get_mut_if` promote LRU recency only when the predicate reports the entry
        // live, so an expired entry is neither promoted nor removed here -- exactly the intent
        // of the old peek-then-get pair, at half the lookups. `track_hit_miss` is disabled on
        // the inner `LruCache`, so no counter is touched by this probe.
        // expired = None (never-expires) -> live; Some(t) -> live while now < t
        let value = if refresh {
            guard
                .get_mut_if(k, |e| e.expires_at.is_none_or(|t| now < t))
                .map(|e| {
                    e.expires_at = self.compute_expires_at(now).or(e.expires_at);
                    e.value.clone()
                })
        } else {
            guard
                .get_if(k, |e| e.expires_at.is_none_or(|t| now < t))
                .map(|e| e.value.clone())
        };
        if let Some(value) = value {
            drop(guard);
            shard.hits.fetch_add(1, Ordering::Relaxed);
            return Ok(Some(value));
        }

        // Not a live hit: either expired (still stored) or absent. `pop_raw` distinguishes
        // them -- `Some` means it was expired and is now removed, `None` means absent. This
        // costs the (rarer) miss path one extra probe to spare every hit one.
        // `pop_raw` bypasses `on_evict` (unlike `cache_remove_entry`); we fire the callback
        // manually below, after releasing the shard lock.
        let removed = guard.pop_raw(k);
        drop(guard);
        if let Some((ref ek, ref entry)) = removed {
            // Count BEFORE notifying: a panicking callback must never leave an
            // entry removed-but-uncounted.
            shard.evictions.fetch_add(1, Ordering::Relaxed);
            if let Some(cb) = &self.inner.on_evict {
                cb(ek, &entry.value);
            }
        }
        shard.misses.fetch_add(1, Ordering::Relaxed);
        Ok(None)
    }

    fn cache_set(&self, k: K, v: V) -> Result<Option<V>, Self::Error> {
        let shard = self.shard_of(&k);
        let now = Instant::now();
        let expires_at = self.compute_expires_at(now);
        let new_entry = TimedEntry {
            expires_at,
            value: v,
        };
        // Capture the displaced entry and evaluate expiry against the `now` sampled above for
        // this operation (B2: a single sample, taken before the lock, cannot see the entry
        // cross the expiry threshold part-way through the op). When an `on_evict` callback is
        // configured we need the *stored* key to hand to it, so the write goes through
        // `cache_set_returning_entry`; otherwise a plain set. Both promote an overwritten key
        // to MRU, so the two branches agree on eviction order. The entry count is unchanged,
        // no capacity eviction is triggered.
        let old: Option<(Option<K>, TimedEntry<V>, bool)> = if self.inner.on_evict.is_some() {
            let mut guard = shard.lock.write();
            guard
                .cache_set_returning_entry(k, new_entry)
                .map(|(ok, e)| {
                    let expired = e.expires_at.is_some_and(|t| now >= t);
                    (Some(ok), e, expired)
                })
        } else {
            shard.lock.write().cache_set(k, new_entry).map(|e| {
                let expired = e.expires_at.is_some_and(|t| now >= t);
                (None, e, expired)
            })
        };
        match old {
            // A displaced expired value is filtered from the return (matching cache_remove and
            // the single-owner TTL stores); fire on_evict and count an eviction for it.
            Some((key, entry, true)) => {
                // Count BEFORE notifying: a panicking callback must never leave an
                // entry removed-but-uncounted.
                shard.evictions.fetch_add(1, Ordering::Relaxed);
                if let (Some(on_evict), Some(key)) = (&self.inner.on_evict, &key) {
                    on_evict(key, &entry.value);
                }
                Ok(None)
            }
            Some((_, entry, false)) => Ok(Some(entry.value)),
            None => Ok(None),
        }
    }

    fn cache_remove(&self, k: &K) -> Result<Option<V>, Self::Error> {
        let shard = self.shard_of(k);
        let removed = shard.lock.write().pop_raw(k);
        if let Some((key, entry)) = removed {
            shard.evictions.fetch_add(1, Ordering::Relaxed);
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
            shard.evictions.fetch_add(1, Ordering::Relaxed);
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
            // The shard's non-capacity eviction counter (expiry / removes / retain / clear).
            shard.evictions.store(0, Ordering::Relaxed);
            // Zero the per-shard inner store's metrics, including its LRU capacity-eviction counter.
            shard.lock.write().cache_reset_metrics();
        }
        Ok(())
    }

    /// Efficient peek-based contains: acquires a read lock, does not clone the value, does not
    /// update LRU recency, and does not record hit/miss metrics. Returns `true` only for live
    /// (not expired) entries.
    fn cache_contains(&self, k: &K) -> Result<bool, Self::Error> {
        let shard = self.shard_of(k);
        let guard = shard.lock.read();
        Ok(guard
            .cache_peek(k)
            .is_some_and(|entry| entry.expires_at.is_none_or(|t| Instant::now() < t)))
    }
}

impl<K, V, H> ConcurrentCachePeek<K, V> for ShardedLruTtlCache<K, V, H>
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
impl<K, V, H> ConcurrentCachePeekAsync<K, V> for ShardedLruTtlCache<K, V, H>
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
impl<K, V, H> ConcurrentCachedAsync<K, V> for ShardedLruTtlCache<K, V, H>
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

    /// Efficient peek-based contains: does not clone the value, does not update LRU recency,
    /// does not record hit/miss metrics, and returns `true` only for live (not expired) entries.
    fn async_cache_contains(&self, k: &K) -> impl Future<Output = Result<bool, Self::Error>> + Send
    where
        Self: Sized + Sync,
        K: Sync,
    {
        let result = ConcurrentCached::cache_contains(self, k);
        async move { result }
    }
}

/// Builder for [`ShardedLruTtlCache`].
///
/// The hasher `H` is the third type parameter, matching every other builder in the crate
/// (`ShardedLruCacheBuilder<K, V, H>`, `ShardedTtlCacheBuilder<K, V, H>`, and so on) and
/// [`LruTtlCacheBuilder`](crate::LruTtlCacheBuilder)`<K, V, S, E>`, so
/// `ShardedLruTtlCacheBuilder<K, V, MyHasher>` names a hasher rather than silently binding
/// `MyHasher` to a typestate slot.
///
/// The trailing type parameter `E` is a **typestate** marker: it starts as [`NoEvict`] and
/// transitions to [`HasEvict`] after `.on_evict(…)` is called. This encodes at compile time
/// whether an eviction callback has been registered, allowing the two `build()` / `copy_from()`
/// overloads to impose `K: 'static + V: 'static` bounds only when `on_evict` is set. You will
/// see this parameter in IDE completions and compiler errors once you call `.on_evict(…)`;
/// it is otherwise invisible.
pub struct ShardedLruTtlCacheBuilder<K, V, H = DefaultShardHasher, E = NoEvict> {
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

impl<K, V> ShardedLruTtlCacheBuilder<K, V> {
    /// Create a builder with default settings. Equivalent to [`ShardedLruTtlCache::builder`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl<K, V, H, E> ShardedLruTtlCacheBuilder<K, V, H, E> {
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
    /// small values. On the default shard-count path the shard count itself is scaled down for
    /// small `max_size` (roughly `max_size / 16`, capped by the CPU-derived default), so the
    /// overshoot stays modest: `max_size = 100` builds 8 shards x 16 = 128 effective capacity.
    /// An explicit [`shards`](Self::shards) count opts out of that scaling, so a large explicit
    /// count with a small `max_size` still overshoots by `shards * 16` (e.g. `max_size = 10`
    /// with an explicit 8 shards yields 128).
    /// [`metrics()`](ShardedLruTtlCache::metrics)'s `capacity` and `entry_count` reflect the
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
    pub fn hasher<H2: ShardHasher<K>>(self, hasher: H2) -> ShardedLruTtlCacheBuilder<K, V, H2, E> {
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

    fn validated_parts(&self) -> Result<(Duration, usize, usize, usize), BuildError> {
        let ttl = self.ttl.ok_or(BuildError::MissingRequired("ttl"))?;
        crate::stores::validate_ttl(ttl)?;
        let n = self.resolve_shard_count()?;
        let mask = n - 1;
        let per_shard_cap = self.resolve_per_shard_cap(n)?;
        let total_cap = self.total_capacity(n, per_shard_cap)?;
        Ok((ttl, mask, per_shard_cap, total_cap))
    }
}

impl<K, V, H> ShardedLruTtlCacheBuilder<K, V, H, NoEvict> {
    /// Set a callback invoked when an entry is evicted by LRU capacity pressure,
    /// TTL-expiry sweeps via [`evict`](ShardedLruTtlCache::evict), explicit
    /// [`cache_remove`](ConcurrentCached::cache_remove) or
    /// [`cache_remove_entry`](ConcurrentCached::cache_remove_entry), and on
    /// [`cache_set`](ConcurrentCached::cache_set) when the displaced entry is already expired.
    /// Does **not** fire on [`clear`](ShardedLruTtlCache::clear);
    /// use [`cache_clear_with_on_evict`](ShardedLruTtlCache::cache_clear_with_on_evict) to opt in.
    ///
    /// Capacity-eviction callbacks run while the affected shard's write lock is held. Do not call
    /// methods on the same sharded cache from the callback; doing so can deadlock if the callback
    /// re-enters the locked shard. TTL expiry sweeps via
    /// [`evict`](ShardedLruTtlCache::evict) and explicit removes via
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
    ) -> ShardedLruTtlCacheBuilder<K, V, H, HasEvict> {
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
    /// Use [`ShardedLruTtlCache::builder()`] to obtain a builder, set at least
    /// [`max_size`](Self::max_size) and [`ttl`](Self::ttl), then call `.build()`.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError`] if `size` (or `per_shard_max_size`) or `ttl` was not set, is `0`,
    /// or if both `max_size` and `per_shard_max_size` are set simultaneously. May also return
    /// [`BuildError::InvalidValue`] if the effective sharded capacity overflows `usize` or a
    /// per-shard allocation fails.
    #[must_use = "the Result from build() must be used"]
    pub fn build(self) -> Result<ShardedLruTtlCache<K, V, H>, BuildError>
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

        Ok(ShardedLruTtlCache {
            inner: Arc::new(LruTtlInner {
                shards,
                shard_mask: mask,
                hasher: self
                    .hasher
                    .expect("hasher is always initialized via Default or .hasher()"),
                on_evict: None,
                ttl_nanos: AtomicU64::new(encode_ttl(ttl)),
                refresh: AtomicBool::new(self.refresh),
                total_capacity: AtomicUsize::new(total_cap),
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
        existing: &ShardedLruTtlCache<K, V, H2>,
    ) -> Result<ShardedLruTtlCache<K, V, H>, BuildError>
    where
        K: Clone + Hash + Eq,
        V: Clone,
        H: ShardHasher<K>,
    {
        Ok(copy_from_lru_ttl(self.build()?, existing))
    }
}

impl<K, V, H> ShardedLruTtlCacheBuilder<K, V, H, HasEvict> {
    /// Build the cache, returning an error if required fields are missing or invalid.
    ///
    /// Use [`ShardedLruTtlCache::builder()`] to obtain a builder, set at least
    /// [`max_size`](Self::max_size) and [`ttl`](Self::ttl), then call `.build()`.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError`] if `size` (or `per_shard_max_size`) or `ttl` was not set, is `0`,
    /// or if both `max_size` and `per_shard_max_size` are set simultaneously. May also return
    /// [`BuildError::InvalidValue`] if the effective sharded capacity overflows `usize` or a
    /// per-shard allocation fails.
    #[must_use = "the Result from build() must be used"]
    pub fn build(self) -> Result<ShardedLruTtlCache<K, V, H>, BuildError>
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

        Ok(ShardedLruTtlCache {
            inner: Arc::new(LruTtlInner {
                shards,
                shard_mask: mask,
                hasher: self
                    .hasher
                    .expect("hasher is always initialized via Default or .hasher()"),
                on_evict: self.on_evict,
                ttl_nanos: AtomicU64::new(encode_ttl(ttl)),
                refresh: AtomicBool::new(self.refresh),
                total_capacity: AtomicUsize::new(total_cap),
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
        existing: &ShardedLruTtlCache<K, V, H2>,
    ) -> Result<ShardedLruTtlCache<K, V, H>, BuildError>
    where
        K: Clone + Hash + Eq + 'static,
        V: Clone + 'static,
        H: ShardHasher<K>,
    {
        Ok(copy_from_lru_ttl(self.build()?, existing))
    }
}

fn copy_from_lru_ttl<K, V, H, H2>(
    new_cache: ShardedLruTtlCache<K, V, H>,
    existing: &ShardedLruTtlCache<K, V, H2>,
) -> ShardedLruTtlCache<K, V, H>
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
            guard.iter_order_raw()
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

impl<K, V, H> ConcurrentCloneCached<K, V> for ShardedLruTtlCache<K, V, H>
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
        // One clock sample per operation, taken before the lock (see `cache_get`).
        let now = Instant::now();
        let mut guard = shard.lock.write();
        // Common case (live hit) in a single lookup: `get_if`/`get_mut_if` promote LRU
        // recency only when the predicate reports the entry live, and leave it in place
        // (no removal, no promotion) when it reports expired. The rarer expired/absent
        // case then takes one extra peek to recover the stale value without removing it.
        let live = if refresh {
            guard
                .get_mut_if(k, |e| e.expires_at.is_none_or(|t| now < t))
                .map(|e| {
                    e.expires_at = self.compute_expires_at(now).or(e.expires_at);
                    e.value.clone()
                })
        } else {
            guard
                .get_if(k, |e| e.expires_at.is_none_or(|t| now < t))
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

impl<K, V, H> ConcurrentCacheExpiry<K, V> for ShardedLruTtlCache<K, V, H>
where
    K: Hash + Eq + Clone,
    V: Clone,
    H: ShardHasher<K>,
{
    /// Returns the stored value and its expiry instant, with no read side effects.
    ///
    /// Takes only a read lock and does not promote LRU recency. The instant is the entry's own
    /// deadline, `None` when the entry never expires (TTL was disabled at insert time). An
    /// extreme ttl is clamped to `u64::MAX` nanoseconds rather than overflowing, so it reports a
    /// real far-future deadline, never `None`. An expired entry is returned with its past
    /// deadline and is **not** removed; the hits/misses counters, the LRU order, and the TTL are
    /// untouched.
    ///
    /// The convention is `now >= t` means expired: a deadline exactly equal to the current
    /// instant counts as already past, matching the liveness check the store itself applies.
    fn cache_peek_expires_at(&self, k: &K) -> (Option<V>, Option<Instant>) {
        let shard = self.shard_of(k);
        let guard = shard.lock.read();
        match guard.cache_peek(k) {
            None => (None, None),
            Some(entry) => (Some(entry.value.clone()), entry.expires_at),
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
    fn default_shard_count_scales_with_max_size() {
        use crate::stores::sharded::{default_shard_count, default_shard_count_for_capacity};
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(100)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        let expected = default_shard_count_for_capacity(Some(100));
        assert_eq!(c.shards(), expected);
        // 100/16 == 6, next_power_of_two(6) == 8, clamped into [1, default_shard_count()].
        assert_eq!(expected, 8usize.clamp(1, default_shard_count()));
        assert_eq!(c.capacity(), c.shards() * 16);
    }

    #[test]
    fn explicit_shards_override_capacity_default() {
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .shards(64)
            .max_size(100)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        assert_eq!(c.shards(), 64);
        assert_eq!(c.capacity(), 64 * 16);
    }

    #[test]
    fn per_shard_max_size_keeps_plain_default_shard_count() {
        use crate::stores::sharded::default_shard_count;
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .per_shard_max_size(4)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        assert_eq!(c.shards(), default_shard_count());
    }

    #[test]
    fn default_shard_count_clamps_at_upper_bound_end_to_end() {
        // This builder has its own resolve_shard_count copy; verify the large-max_size clamp
        // reaches default_shard_count() through it, with the expectation computed at runtime.
        use crate::stores::sharded::default_shard_count;
        let d = default_shard_count();
        let big = d.checked_mul(16).unwrap().checked_mul(4).unwrap();
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(big)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        assert_eq!(c.shards(), d);
    }

    #[test]
    fn cache_set_over_expired_returns_none_fires_on_evict_and_counts() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU64, Ordering as AOrd};
        let count = Arc::new(AtomicU64::new(0));
        let count2 = count.clone();
        let c = ShardedLruTtlCache::<u32, u32>::builder()
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
    fn builder_generic_param_order_is_hasher_then_eviction_typestate() {
        // API-5: ShardedLruTtlCacheBuilder's params are <K, V, H, E> (hasher third, eviction
        // typestate last), matching every other builder in the crate and
        // LruTtlCacheBuilder<K, V, S, E>. Naming them positionally in that order must compile;
        // this pins the order against reordering.
        let _default: ShardedLruTtlCacheBuilder<u32, u32, DefaultShardHasher, NoEvict> =
            ShardedLruTtlCache::<u32, u32>::builder();

        // Naming only the hasher must resolve to the hasher slot, not the typestate slot: this
        // is the spelling a user reaches for, and it silently bound to `E` before the reorder.
        let _hasher_only: ShardedLruTtlCacheBuilder<u32, u32, DefaultShardHasher> =
            ShardedLruTtlCache::<u32, u32>::builder();

        // A custom hasher slots into the third position, and .on_evict flips the typestate to
        // HasEvict (last position) while the hasher stays third.
        let cache = ShardedLruTtlCache::<u32, u32>::builder()
            .shards(1)
            .max_size(8)
            .ttl(Duration::from_secs(60))
            .hasher(DefaultShardHasher::default())
            .on_evict(|_, _| {})
            .build()
            .unwrap();
        let _typed: ShardedLruTtlCache<u32, u32, DefaultShardHasher> = cache;
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
        let c = ShardedLruTtlCache::<u32, u32>::builder()
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
        let c = ShardedLruTtlCache::<u32, u32>::builder()
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
        let err = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(100)
            .per_shard_max_size(10)
            .ttl(Duration::from_secs(60))
            .build();
        assert!(err.is_err());
    }

    #[test]
    fn build_rejects_overflowing_shards_and_capacity() {
        let err = ShardedLruTtlCache::<u32, u32>::builder()
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

        let err = ShardedLruTtlCache::<u32, u32>::builder()
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
        let cache: ShardedLruTtlCache<&str, &str> = ShardedLruTtlCache::builder()
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
    fn try_set_ttl_rejects_zero_and_returns_previous() {
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(64)
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
        let old = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(64)
            .ttl(Duration::from_millis(50))
            .build()
            .unwrap();
        for i in 0..10u32 {
            SyncConcurrentCached::cache_set(&old, i, i).expect("insert must succeed");
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        let new_cache = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(64)
            .ttl(Duration::from_secs(60))
            .copy_from(&old)
            .unwrap();
        assert_eq!(new_cache.len(), 0);
    }

    #[test]
    fn copy_from_preserves_live_entries() {
        // Use shards(1) to avoid per-shard capacity eviction during insertion.
        let old = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(1024)
            .shards(1)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        for i in 0..20u32 {
            SyncConcurrentCached::cache_set(&old, i, i * 10).expect("insert must succeed");
        }
        let new_cache = ShardedLruTtlCache::<u32, u32>::builder()
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
        let old = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(64)
            .shards(1)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        for i in 0..32u32 {
            SyncConcurrentCached::cache_set(&old, i, i).expect("insert must succeed");
        }
        let new_cache = ShardedLruTtlCache::<u32, u32>::builder()
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
        let err = ShardedLruTtlCache::<u32, u32>::builder()
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
        let c = ShardedLruTtlCache::<u32, u32>::builder()
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
        let c = ShardedLruTtlCache::<u32, u32>::builder()
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
        let c = ShardedLruTtlCache::<u32, u32>::builder()
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
        let c = ShardedLruTtlCache::<u32, u32>::builder()
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
        let c = ShardedLruTtlCache::<u32, u32>::builder()
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
        let c = ShardedLruTtlCache::<u32, u32>::builder()
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
        let c = ShardedLruTtlCache::<u32, u32>::builder()
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
        let c = ShardedLruTtlCache::<u32, u32>::builder()
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
        let c = ShardedLruTtlCache::<u32, u32>::builder()
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
        let c = ShardedLruTtlCache::<u32, u32>::builder()
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
        let c = ShardedLruTtlCache::<u32, u32>::builder()
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

    // --- ConcurrentCacheExpiry tests ---

    #[test]
    fn peek_expires_at_absent_key_returns_none_none() {
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(64)
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
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(64)
            .ttl(ttl)
            .build()
            .unwrap();
        let before = Instant::now();
        SyncConcurrentCached::cache_set(&c, 1, 10).expect("insert must succeed");
        let after = Instant::now();

        let (value, expires_at) = ConcurrentCacheExpiry::cache_peek_expires_at(&c, &1);
        assert_eq!(value, Some(10));
        let expires_at = expires_at.expect("a configured ttl must record a deadline");
        assert!(expires_at > Instant::now(), "a live entry expires later");
        assert!(expires_at >= before + ttl && expires_at <= after + ttl);
    }

    #[test]
    fn peek_expires_at_never_expiring_entry_reports_no_deadline() {
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(64)
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
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(64)
            .ttl(Duration::from_millis(50))
            .shards(1)
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1, 10).expect("insert must succeed");
        std::thread::sleep(std::time::Duration::from_millis(100));

        let (value, expires_at) = ConcurrentCacheExpiry::cache_peek_expires_at(&c, &1);
        assert_eq!(value, Some(10), "an expired entry is still returned");
        let deadline = expires_at.expect("an expired entry still carries its past deadline");
        assert!(deadline <= Instant::now(), "the past deadline is reported");

        // Not removed by the peek, and no eviction counted.
        assert_eq!(c.len(), 1);
        assert_eq!(c.metrics().evictions, Some(0));
        assert_eq!(
            ConcurrentCacheExpiry::cache_peek_expires_at(&c, &1),
            (Some(10), Some(deadline))
        );
    }

    #[test]
    fn peek_expires_at_does_not_touch_hit_or_miss_counters() {
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(64)
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
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(64)
            .ttl(Duration::from_millis(200))
            .refresh_on_hit(true)
            .build()
            .unwrap();
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

    #[test]
    fn peek_expires_at_does_not_promote_lru() {
        // max_size(2) + shards(1): with only 2 slots, inserting a third entry evicts the
        // LRU entry. If the peek promoted recency, it would change which entry survives.
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(2)
            .ttl(Duration::from_secs(60))
            .shards(1)
            .build()
            .unwrap();

        SyncConcurrentCached::cache_set(&c, 1u32, 10u32).expect("insert must succeed");
        SyncConcurrentCached::cache_set(&c, 2u32, 20u32).expect("insert must succeed");

        // Peek key 1 -- must NOT promote it to MRU.
        let (value, expires_at) = ConcurrentCacheExpiry::cache_peek_expires_at(&c, &1u32);
        assert_eq!(value, Some(10));
        assert!(expires_at.is_some());

        // Inserting key 3 must evict key 1 (still LRU because the peek did not promote it).
        SyncConcurrentCached::cache_set(&c, 3u32, 30u32).expect("insert must succeed");

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
    fn peek_expires_at_multi_shard() {
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(256)
            .ttl(Duration::from_secs(3600))
            .shards(8)
            .build()
            .unwrap();
        assert_eq!(c.shards(), 8);

        for i in 0..32u32 {
            SyncConcurrentCached::cache_set(&c, i, i * 10).expect("insert must succeed");
        }
        for i in 0..32u32 {
            let (value, expires_at) = ConcurrentCacheExpiry::cache_peek_expires_at(&c, &i);
            assert_eq!(value, Some(i * 10), "key {i} must route to the right shard");
            assert!(expires_at.is_some_and(|t| t > Instant::now()));
        }
        assert_eq!(
            ConcurrentCacheExpiry::cache_peek_expires_at(&c, &999u32),
            (None, None)
        );
    }

    /// Force a stored entry's `expires_at`, bypassing the TTL stamping. Mirrors the
    /// `set_expiry` test helper in `ShardedTtlCache`'s test suite.
    fn set_expiry<H: ShardHasher<u32>>(
        c: &ShardedLruTtlCache<u32, u32, H>,
        k: u32,
        expires_at: Option<Instant>,
    ) {
        let shard = c.shard_of(&k);
        let mut guard = shard.lock.write();
        guard
            .get_mut_if(&k, |_| true)
            .expect("key stored")
            .expires_at = expires_at;
    }

    // Certification gap (Q2): `ShardedLruTtlCache` is a real TTL store -- its deadline is the
    // entry's own stored `expires_at`, the exact same field `cache_peek_with_expiry_status`
    // consults, so (unlike the `Expires`-based sharded stores) the two must agree exactly at the
    // boundary. Mirrors `ShardedTtlCache`'s
    // `peek_expires_at_deadline_is_past_exactly_when_peek_reports_expired`.
    #[test]
    fn peek_expires_at_deadline_is_past_exactly_when_peek_reports_expired() {
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(64)
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

    // Gap: the crate's documented convention is `now >= expires_at` means expired. Pin that
    // `peek_expires_at`'s raw deadline and `cache_peek_with_expiry_status`'s liveness judgement
    // agree exactly at the tie, deterministically (no sleep).
    #[test]
    fn peek_expires_at_boundary_matches_now_ge_expires_at_convention() {
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(64)
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

    // Certification gap (extreme-TTL question): the sharded stores clamp `ttl_nanos` to
    // `u64::MAX` before `compute_expires_at` ever runs, so a `Duration::MAX` ttl does NOT reach
    // the overflow branch the single-owner `LruTtlCache` hits (which yields `expires_at = None`).
    // Pin the actual observable behavior here: a real, very distant deadline, not `None`.
    #[test]
    fn peek_expires_at_extreme_ttl_is_clamped_not_overflowed() {
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(64)
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
            "an extreme ttl is clamped to ~584 years, not overflowed to None, unlike LruTtlCache",
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

    // Gap: changing the store's ttl (including disabling it) must NOT retroactively touch a
    // deadline an already-stored entry carries -- `set_ttl`/`unset_ttl` only swap the shared
    // `ttl_nanos` atomic and never walk existing entries.
    #[test]
    fn peek_expires_at_reports_stale_deadline_after_ttl_change() {
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(64)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1, 100).expect("insert must succeed");
        let (_, original) = ConcurrentCacheExpiry::cache_peek_expires_at(&c, &1);
        let original = original.expect("a configured ttl must record a deadline");

        c.set_ttl(Duration::from_secs(5));
        assert_eq!(
            ConcurrentCacheExpiry::cache_peek_expires_at(&c, &1),
            (Some(100), Some(original)),
            "changing the store ttl must not retroactively rewrite an existing entry's deadline"
        );

        c.unset_ttl();
        assert_eq!(
            ConcurrentCacheExpiry::cache_peek_expires_at(&c, &1),
            (Some(100), Some(original)),
            "disabling the ttl must not clear an already-stored entry's deadline"
        );

        SyncConcurrentCached::cache_set(&c, 2, 200).expect("insert must succeed");
        assert_eq!(
            ConcurrentCacheExpiry::cache_peek_expires_at(&c, &2),
            (Some(200), None)
        );
    }

    #[test]
    fn peek_expires_at_reports_absent_after_evict_removes_the_entry() {
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(64)
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
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(64)
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

    // Gap: the ergonomic alias must agree with the canonical method across every return shape
    // the contract defines, not just the absent-key case the implementor already covered.
    #[test]
    fn peek_expires_at_alias_matches_canonical_across_all_return_shapes() {
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(64)
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

    // Gap: nothing else in the suite calls `ConcurrentCacheExpiry` through a generic
    // `T: ConcurrentCacheExpiry<K, V>` bound or through `cached::prelude::*` -- both a
    // monomorphization/dyn-compat regression and a prelude export regression would go
    // uncaught otherwise.
    #[test]
    fn concurrent_cache_expiry_is_reachable_through_a_generic_bound_and_the_prelude() {
        use crate::prelude::*;

        fn peek_via_bound<T: ConcurrentCacheExpiry<u32, u32>>(
            store: &T,
            key: &u32,
        ) -> (Option<u32>, Option<crate::time::Instant>) {
            store.cache_peek_expires_at(key)
        }

        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(64)
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

    /// Keys `0..8` expired, `8..16` live, `16..24` never-expires (`expires_at == None`, what
    /// `unset_ttl` stamps) -- all three kinds mixed into the same shards.
    fn populate_mixed<H: ShardHasher<u32>>(c: &ShardedLruTtlCache<u32, u32, H>) {
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

    // Certification gap (Q3): existing `peek_expires_at` tests only ever route a single
    // uniform group of keys. Route several distinct keys with a MIX of expired / live /
    // never-expiring deadlines through a multi-shard cache and confirm each is read back
    // correctly, regardless of which physical shard it landed in.
    #[test]
    fn peek_expires_at_routes_correctly_across_many_shards() {
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(256)
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

    // Certification gap (read-lock-only property): a writer on one thread (through an
    // Arc-shared clone) updates a key's deadline; a subsequent peek on the original handle,
    // after the writer has joined, must observe that update -- exercising cross-thread
    // visibility through the shard lock rather than the single-threaded round-trips every
    // other `peek_expires_at` test performs.
    #[test]
    fn peek_expires_at_observes_a_concurrent_writers_deadline_update() {
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(64)
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

    #[test]
    fn retain_preserves_survivor_recency_order() {
        // shards(1) so the recency order is deterministic and observable from one shard.
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .shards(1)
            .max_size(64)
            .ttl(Duration::from_secs(60))
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

    /// Counter-wiring contract (the main correctness trap for this store): `retain` must
    /// bump the per-shard non-capacity eviction counter (`Shard::evictions`), NOT the inner
    /// `LruCache`'s own capacity-eviction counter (`guard.evictions`), which `evict()` also
    /// leaves untouched.
    #[test]
    fn retain_wires_to_non_capacity_evictions_not_inner_lru_counter() {
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .shards(1)
            .max_size(64)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        for i in 0..10u32 {
            SyncConcurrentCached::cache_set(&c, i, i).expect("insert must succeed");
        }
        let inner_before = c.inner.shards[0]
            .lock
            .read()
            .evictions
            .load(Ordering::Relaxed);
        let outer_before = c.non_capacity_evictions();

        c.retain(|k, _| k % 2 == 0);

        let inner_after = c.inner.shards[0]
            .lock
            .read()
            .evictions
            .load(Ordering::Relaxed);
        let outer_after = c.non_capacity_evictions();

        assert_eq!(
            inner_after, inner_before,
            "retain must not touch the inner LRU capacity-eviction counter"
        );
        assert_eq!(
            outer_after - outer_before,
            5,
            "retain must count each removal via non_capacity_evictions"
        );
        // metrics() sums inner + outer; the combined total must also reflect exactly the
        // removed count with no double counting.
        assert_eq!(
            c.metrics().evictions.unwrap() - (inner_before + outer_before),
            5
        );
    }

    /// Both `retain` and `evict` decide expiry with the same `now >= expires_at`
    /// comparison. This pins an entry's `expires_at` to the instant just sampled (rather
    /// than backdating it, which the other expiry tests already cover) and checks that
    /// `evict()` and `retain()` both remove it. Because `Instant` is monotonic, the `now`
    /// sampled a moment later inside `evict`/`retain` is always `>=` the `expires_at`
    /// captured here, so both paths must treat it as expired -- this is the closest a
    /// real-clock test can get to exercising the literal `now == expires_at` tie without a
    /// mock clock.
    #[test]
    fn retain_and_evict_agree_at_the_expires_at_equals_now_boundary() {
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .shards(1)
            .max_size(64)
            .ttl(Duration::from_secs(3600))
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1, 10).expect("insert must succeed");
        SyncConcurrentCached::cache_set(&c, 2, 20).expect("insert must succeed");

        // evict() path: pin key 1's expiry to "now".
        {
            let shard = c.shard_of(&1);
            let mut guard = shard.lock.write();
            let entry = guard.get_mut_if(&1, |_| true).expect("key 1 stored");
            entry.expires_at = Some(Instant::now());
        }
        let removed = c.evict();
        assert_eq!(
            removed, 1,
            "an entry whose expires_at is the just-sampled now must be swept by evict()"
        );
        assert_eq!(SyncConcurrentCached::cache_get(&c, &1).unwrap(), None);
        assert_eq!(SyncConcurrentCached::cache_get(&c, &2).unwrap(), Some(20));

        // retain() path: symmetric case with the surviving entry pinned the same way.
        {
            let shard = c.shard_of(&2);
            let mut guard = shard.lock.write();
            let entry = guard.get_mut_if(&2, |_| true).expect("key 2 stored");
            entry.expires_at = Some(Instant::now());
        }
        c.retain(|_k, _v| true);
        assert_eq!(
            SyncConcurrentCached::cache_get(&c, &2).unwrap(),
            None,
            "an entry whose expires_at is the just-sampled now must be swept by retain() too, \
             even under a keep-everything predicate"
        );
    }

    // --- single-lookup `cache_get`, per-shard eviction counters, and write recency ---

    /// Raw per-shard non-capacity eviction counters, in shard order.
    fn shard_eviction_counters<K, V, H>(c: &ShardedLruTtlCache<K, V, H>) -> Vec<u64> {
        c.inner
            .shards
            .iter()
            .map(|s| s.evictions.load(Ordering::Relaxed))
            .collect()
    }

    /// Index of the shard that owns `k`.
    fn owning_shard<K, V, H: ShardHasher<K>>(c: &ShardedLruTtlCache<K, V, H>, k: &K) -> usize {
        shard_index(c.inner.hasher.shard_hash(k), c.inner.shard_mask)
    }

    /// Keys of one shard in MRU -> LRU order.
    fn shard_key_order<K: Clone + Hash + Eq, V: Clone, H>(
        c: &ShardedLruTtlCache<K, V, H>,
        shard: usize,
    ) -> Vec<K> {
        c.inner.shards[shard]
            .lock
            .read()
            .iter_order_raw()
            .into_iter()
            .map(|(k, _)| k)
            .collect()
    }

    /// `cache_get` resolves a live hit in a single hash + probe (`get_if`) and falls through to
    /// `pop_raw` only when that probe fails. All four outcomes must keep their old semantics:
    /// live hit -> value + hit; absent -> None + miss, nothing removed; expired -> None + miss,
    /// entry removed, one eviction counted, `on_evict` fired with the stored key.
    #[test]
    fn cache_get_single_lookup_keeps_hit_miss_expired_and_absent_semantics() {
        use std::sync::Mutex;
        let seen: Arc<Mutex<Vec<(u32, u32)>>> = Arc::new(Mutex::new(Vec::new()));
        let seen2 = Arc::clone(&seen);
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .shards(1)
            .max_size(8)
            .ttl(Duration::from_millis(20))
            .on_evict(move |k: &u32, v: &u32| seen2.lock().unwrap().push((*k, *v)))
            .build()
            .unwrap();

        // Live hit.
        SyncConcurrentCached::cache_set(&c, 1, 10).expect("insert must succeed");
        assert_eq!(SyncConcurrentCached::cache_get(&c, &1).unwrap(), Some(10));
        assert_eq!(c.metrics().hits, Some(1));
        assert_eq!(c.metrics().misses, Some(0));
        assert_eq!(c.len(), 1, "a live hit must not remove anything");

        // Absent key: a miss, and the fall-through `pop_raw` must remove nothing.
        assert_eq!(SyncConcurrentCached::cache_get(&c, &404).unwrap(), None);
        assert_eq!(c.metrics().hits, Some(1));
        assert_eq!(c.metrics().misses, Some(1));
        assert_eq!(c.len(), 1);
        assert!(seen.lock().unwrap().is_empty(), "no eviction yet");
        let evictions_before = c.metrics().evictions.expect("evictions tracked");

        // Expired key: a miss, removed from the store, counted, callback fired.
        std::thread::sleep(std::time::Duration::from_millis(60));
        assert_eq!(SyncConcurrentCached::cache_get(&c, &1).unwrap(), None);
        assert_eq!(c.metrics().hits, Some(1));
        assert_eq!(c.metrics().misses, Some(2));
        assert_eq!(c.len(), 0, "the expired entry must be removed on access");
        assert_eq!(*seen.lock().unwrap(), vec![(1, 10)]);
        assert_eq!(
            c.metrics().evictions.expect("evictions tracked") - evictions_before,
            1,
            "lazy expiry must count exactly one eviction"
        );
        // A second read of the now-absent key is a plain miss with no extra eviction.
        assert_eq!(SyncConcurrentCached::cache_get(&c, &1).unwrap(), None);
        assert_eq!(c.metrics().misses, Some(3));
        assert_eq!(
            c.metrics().evictions.expect("evictions tracked") - evictions_before,
            1
        );
    }

    /// The single-lookup path must still promote what it reads: `get_if`/`get_mut_if` move the
    /// entry to MRU when (and only when) the predicate reports it live. Checked directly on the
    /// recency chain and through the capacity eviction it decides, with and without
    /// `refresh_on_hit`.
    #[test]
    fn cache_get_promotes_recency_through_the_single_lookup_path() {
        for refresh in [false, true] {
            let c = ShardedLruTtlCache::<u32, u32>::builder()
                .shards(1)
                .max_size(2)
                .ttl(Duration::from_secs(3600))
                .refresh_on_hit(refresh)
                .build()
                .unwrap();
            SyncConcurrentCached::cache_set(&c, 1, 10).expect("insert must succeed");
            SyncConcurrentCached::cache_set(&c, 2, 20).expect("insert must succeed");
            assert_eq!(shard_key_order(&c, 0), vec![2, 1]);

            assert_eq!(SyncConcurrentCached::cache_get(&c, &1).unwrap(), Some(10));
            assert_eq!(
                shard_key_order(&c, 0),
                vec![1, 2],
                "refresh_on_hit={refresh}: a live read must promote the entry to MRU"
            );

            // ... so the next capacity eviction claims key 2, not the just-read key 1.
            SyncConcurrentCached::cache_set(&c, 3, 30).expect("insert must succeed");
            assert!(SyncConcurrentCached::cache_contains(&c, &1).unwrap());
            assert!(!SyncConcurrentCached::cache_contains(&c, &2).unwrap());
        }
    }

    /// With `refresh_on_hit`, the live-hit probe is `get_mut_if` and must still renew the
    /// entry's expiry from the operation's single clock sample.
    ///
    /// The timings leave a wide margin (300 ms TTL vs 100 ms between hits) so a loaded
    /// machine oversleeping a gap cannot make the entry expire mid-loop; the total elapsed
    /// time still exceeds the TTL several times over, so a *missing* refresh fails the loop.
    #[test]
    fn cache_get_refreshes_expiry_through_the_single_lookup_path() {
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .shards(1)
            .max_size(8)
            .ttl(Duration::from_millis(300))
            .refresh_on_hit(true)
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1, 10).expect("insert must succeed");
        for _ in 0..5 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            assert_eq!(
                SyncConcurrentCached::cache_get(&c, &1).unwrap(),
                Some(10),
                "each hit must push the expiry out, so the entry never expires"
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(600));
        assert_eq!(
            SyncConcurrentCached::cache_get(&c, &1).unwrap(),
            None,
            "without a hit the refreshed expiry must still elapse"
        );
    }

    /// `cache_set` with an `on_evict` callback recovers the stored key through
    /// `cache_set_returning_entry`, which promotes the overwritten entry to MRU. This test
    /// fails if that promotion is dropped.
    #[test]
    fn cache_set_with_on_evict_promotes_overwritten_entry_to_mru() {
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .shards(1)
            .max_size(2)
            .ttl(Duration::from_secs(3600))
            .on_evict(|_, _| {})
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1, 10).expect("insert must succeed");
        SyncConcurrentCached::cache_set(&c, 2, 20).expect("insert must succeed");
        assert_eq!(shard_key_order(&c, 0), vec![2, 1]);

        // Overwrite the LRU entry: it must come back as MRU, and return the old value.
        assert_eq!(
            SyncConcurrentCached::cache_set(&c, 1, 11).unwrap(),
            Some(10),
            "overwrite must return the displaced live value"
        );
        assert_eq!(
            shard_key_order(&c, 0),
            vec![1, 2],
            "overwriting an entry must promote it to MRU (as pop-then-insert did)"
        );
        assert_eq!(c.len(), 2, "an overwrite must not change the entry count");

        // The promotion decides the next capacity eviction victim.
        SyncConcurrentCached::cache_set(&c, 3, 30).expect("insert must succeed");
        assert_eq!(
            SyncConcurrentCached::cache_get(&c, &1).unwrap(),
            Some(11),
            "the promoted (overwritten) entry must survive the capacity eviction"
        );
        assert!(!SyncConcurrentCached::cache_contains(&c, &2).unwrap());
    }

    /// The `on_evict`-free `cache_set` path is a plain `LruCache::cache_set`, which now
    /// promotes on overwrite just like the `on_evict` path's `cache_set_returning_entry`.
    /// Attaching a purely observational callback must not change eviction order.
    #[test]
    fn cache_set_without_on_evict_promotes_overwritten_entry() {
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .shards(1)
            .max_size(2)
            .ttl(Duration::from_secs(3600))
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1, 10).expect("insert must succeed");
        SyncConcurrentCached::cache_set(&c, 2, 20).expect("insert must succeed");
        assert_eq!(
            SyncConcurrentCached::cache_set(&c, 1, 11).unwrap(),
            Some(10)
        );
        assert_eq!(
            shard_key_order(&c, 0),
            vec![1, 2],
            "an overwrite promotes to MRU with or without an on_evict callback"
        );

        // ... and the promotion decides the next capacity eviction victim, exactly as it
        // does on the `on_evict` path.
        SyncConcurrentCached::cache_set(&c, 3, 30).expect("insert must succeed");
        assert_eq!(SyncConcurrentCached::cache_get(&c, &1).unwrap(), Some(11));
        assert!(!SyncConcurrentCached::cache_contains(&c, &2).unwrap());
    }

    /// Overwriting the current MRU entry (already the head of the LRU chain) must leave it
    /// at the head with the chain intact, on both `cache_set` branches.
    #[test]
    fn cache_set_over_current_mru_keeps_it_at_the_front() {
        for with_on_evict in [false, true] {
            let builder = ShardedLruTtlCache::<u32, u32>::builder()
                .shards(1)
                .max_size(3)
                .ttl(Duration::from_secs(3600));
            let c = if with_on_evict {
                builder.on_evict(|_, _| {}).build().unwrap()
            } else {
                builder.build().unwrap()
            };
            for k in 1..=3u32 {
                SyncConcurrentCached::cache_set(&c, k, k * 10).expect("insert must succeed");
            }
            assert_eq!(shard_key_order(&c, 0), vec![3, 2, 1]);
            assert_eq!(
                SyncConcurrentCached::cache_set(&c, 3, 33).unwrap(),
                Some(30)
            );
            assert_eq!(
                shard_key_order(&c, 0),
                vec![3, 2, 1],
                "on_evict={with_on_evict}: overwriting the head must keep it at the head"
            );
            assert_eq!(c.len(), 3);
            // The chain is intact: the LRU victim is still key 1.
            SyncConcurrentCached::cache_set(&c, 4, 40).expect("insert must succeed");
            assert_eq!(shard_key_order(&c, 0), vec![4, 3, 2]);
            assert_eq!(SyncConcurrentCached::cache_get(&c, &1).unwrap(), None);
        }
    }

    /// A 1-capacity shard: overwriting the sole entry must not corrupt the chain.
    #[test]
    fn cache_set_over_sole_entry_of_capacity_one_shard() {
        for with_on_evict in [false, true] {
            let builder = ShardedLruTtlCache::<u32, u32>::builder()
                .shards(1)
                .max_size(1)
                .ttl(Duration::from_secs(3600));
            let c = if with_on_evict {
                builder.on_evict(|_, _| {}).build().unwrap()
            } else {
                builder.build().unwrap()
            };
            SyncConcurrentCached::cache_set(&c, 1, 10).expect("insert must succeed");
            assert_eq!(
                SyncConcurrentCached::cache_set(&c, 1, 11).unwrap(),
                Some(10),
                "on_evict={with_on_evict}"
            );
            assert_eq!(shard_key_order(&c, 0), vec![1]);
            assert_eq!(c.len(), 1);
            SyncConcurrentCached::cache_set(&c, 2, 20).expect("insert must succeed");
            assert_eq!(shard_key_order(&c, 0), vec![2]);
            assert_eq!(c.len(), 1);
        }
    }

    /// Every non-capacity eviction is counted on the shard that owns the key, and `metrics()`
    /// aggregates the per-shard counters together with the inner LRU capacity counters without
    /// double-counting.
    #[test]
    fn every_eviction_path_counts_on_the_owning_shard() {
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .shards(8)
            .per_shard_max_size(8)
            .ttl(Duration::from_millis(20))
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
            "cache_get lazy expiry"
        );

        // 2) cache_remove.
        SyncConcurrentCached::cache_set(&c, 2, 20).expect("insert must succeed");
        assert_eq!(
            SyncConcurrentCached::cache_remove(&c, &2).unwrap(),
            Some(20)
        );
        expected[owning_shard(&c, &2)] += 1;
        assert_eq!(shard_eviction_counters(&c), expected, "cache_remove");

        // 3) cache_remove_entry.
        SyncConcurrentCached::cache_set(&c, 3, 30).expect("insert must succeed");
        assert_eq!(
            SyncConcurrentCached::cache_remove_entry(&c, &3).unwrap(),
            Some((3, 30))
        );
        expected[owning_shard(&c, &3)] += 1;
        assert_eq!(shard_eviction_counters(&c), expected, "cache_remove_entry");

        // 4) cache_set displacing an expired entry.
        SyncConcurrentCached::cache_set(&c, 4, 40).expect("insert must succeed");
        std::thread::sleep(std::time::Duration::from_millis(60));
        assert_eq!(SyncConcurrentCached::cache_set(&c, 4, 41).unwrap(), None);
        expected[owning_shard(&c, &4)] += 1;
        assert_eq!(
            shard_eviction_counters(&c),
            expected,
            "cache_set over expired"
        );

        // 5) evict() sweeps key 4 (re-set above) and key 5, both expired by now.
        SyncConcurrentCached::cache_set(&c, 5, 50).expect("insert must succeed");
        std::thread::sleep(std::time::Duration::from_millis(60));
        assert_eq!(c.evict(), 2, "both stored entries are expired");
        expected[owning_shard(&c, &4)] += 1;
        expected[owning_shard(&c, &5)] += 1;
        assert_eq!(shard_eviction_counters(&c), expected, "evict");

        // 6) retain().
        SyncConcurrentCached::cache_set(&c, 6, 60).expect("insert must succeed");
        c.retain(|_k, _v| false);
        expected[owning_shard(&c, &6)] += 1;
        assert_eq!(shard_eviction_counters(&c), expected, "retain");

        // 7) cache_clear_with_on_evict().
        SyncConcurrentCached::cache_set(&c, 7, 70).expect("insert must succeed");
        c.cache_clear_with_on_evict();
        expected[owning_shard(&c, &7)] += 1;
        assert_eq!(
            shard_eviction_counters(&c),
            expected,
            "cache_clear_with_on_evict"
        );

        // The aggregate matches the sum of the per-shard counters exactly: no capacity
        // eviction has happened yet, so nothing else contributes.
        let total: u64 = expected.iter().sum();
        assert_eq!(c.metrics().evictions, Some(total));
        assert_eq!(ConcurrentCacheBase::cache_evictions(&c), Some(total));

        // 8) Capacity evictions stay in the inner per-shard LruCache counter (the other half
        //    of the split) and are added on top by metrics().
        let victims: Vec<u32> = (0..1000u32)
            .filter(|i| owning_shard(&c, i) == 0)
            .take(20)
            .collect();
        assert_eq!(victims.len(), 20, "need 20 keys landing on shard 0");
        for k in &victims {
            SyncConcurrentCached::cache_set(&c, *k, *k).expect("insert must succeed");
        }
        let capacity_evictions = victims.len() as u64 - 8;
        assert!(
            capacity_evictions > 0,
            "the test must actually overflow one shard's capacity"
        );
        assert_eq!(
            shard_eviction_counters(&c),
            expected,
            "capacity evictions must NOT touch the non-capacity counters"
        );
        assert_eq!(
            c.metrics().evictions,
            Some(total + capacity_evictions),
            "metrics() must sum capacity and non-capacity evictions without double counting"
        );

        // cache_reset_metrics zeroes both families.
        ConcurrentCached::cache_reset_metrics(&c).unwrap();
        assert_eq!(shard_eviction_counters(&c), vec![0u64; 8]);
        assert_eq!(c.metrics().evictions, Some(0));
    }

    /// `deep_clone` used to copy one process-wide eviction counter; with the counters
    /// per-shard it must copy each shard's, so the clone reports the same totals.
    #[test]
    fn deep_clone_preserves_per_shard_eviction_counts() {
        // `per_shard_max_size` must be >= the 8 seed keys: the shard hasher is seeded per
        // process, so any smaller cap lets an unlucky distribution (5+ of the 8 landing in one
        // shard) capacity-evict a key before the `cache_remove` loop below reaches it. That
        // remove would then find nothing, count no non-capacity eviction, and the exact
        // assertion further down would fail on roughly one run in eight.
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .shards(4)
            .per_shard_max_size(8)
            .ttl(Duration::from_secs(3600))
            .build()
            .unwrap();
        for i in 0..8u32 {
            SyncConcurrentCached::cache_set(&c, i, i).expect("insert must succeed");
        }
        // Non-capacity evictions (explicit removes) plus capacity evictions from overfilling.
        for i in 0..4u32 {
            let _ = SyncConcurrentCached::cache_remove(&c, &i).unwrap();
        }
        for i in 100..140u32 {
            SyncConcurrentCached::cache_set(&c, i, i).expect("insert must succeed");
        }
        let before = c.metrics().evictions.expect("evictions tracked");
        assert!(before >= 4, "the fixture must produce evictions");
        let per_shard_before = shard_eviction_counters(&c);
        assert_eq!(per_shard_before.iter().sum::<u64>(), 4);

        let cloned = c.deep_clone();
        assert_eq!(
            shard_eviction_counters(&cloned),
            per_shard_before,
            "each shard's non-capacity counter must carry across a deep_clone"
        );
        assert_eq!(
            cloned.metrics().evictions,
            Some(before),
            "the clone must report the same eviction total"
        );

        // The clone is independent: further evictions on it do not touch the source.
        SyncConcurrentCached::cache_set(&cloned, 999, 999).expect("insert must succeed");
        assert_eq!(
            SyncConcurrentCached::cache_remove(&cloned, &999).unwrap(),
            Some(999)
        );
        assert_eq!(
            shard_eviction_counters(&cloned).iter().sum::<u64>(),
            5,
            "the explicit remove must count on the clone's own shard counter"
        );
        assert_eq!(
            shard_eviction_counters(&c),
            per_shard_before,
            "the source's per-shard counters must be untouched by the clone"
        );
        assert_eq!(
            c.metrics().evictions,
            Some(before),
            "the source's totals must be untouched by the clone"
        );
    }

    /// `cache_clear_with_on_evict` drains each shard with `LruCache::drain_all` (no key clones,
    /// no re-hashing); the callbacks must still arrive most-recently-used first, per shard.
    #[test]
    fn cache_clear_with_on_evict_fires_mru_to_lru_per_shard() {
        use std::sync::Mutex;
        let seen: Arc<Mutex<Vec<u32>>> = Arc::new(Mutex::new(Vec::new()));
        let seen2 = Arc::clone(&seen);
        let c = ShardedLruTtlCache::<u32, u32>::builder()
            .shards(1)
            .max_size(64)
            .ttl(Duration::from_secs(3600))
            .on_evict(move |k: &u32, _v: &u32| seen2.lock().unwrap().push(*k))
            .build()
            .unwrap();
        for i in 0..6u32 {
            SyncConcurrentCached::cache_set(&c, i, i).expect("insert must succeed");
        }
        assert_eq!(SyncConcurrentCached::cache_get(&c, &0).unwrap(), Some(0));
        assert_eq!(SyncConcurrentCached::cache_get(&c, &2).unwrap(), Some(2));
        let expected = shard_key_order(&c, 0);
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
        assert!(c.is_empty());
        // The drained shard stays usable.
        SyncConcurrentCached::cache_set(&c, 42, 42).expect("insert must succeed");
        assert_eq!(SyncConcurrentCached::cache_get(&c, &42).unwrap(), Some(42));
    }
}
