use std::borrow::Borrow;
use std::hash::Hash;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use crate::{
    CacheMetrics, CachedIter, CachedPeek, ConcurrentCacheBase, ConcurrentCacheExpiry,
    ConcurrentCachePeek, ConcurrentCached, ConcurrentCloneCached, Expires,
};
#[cfg(feature = "async_core")]
use crate::{ConcurrentCachePeekAsync, ConcurrentCachedAsync};
#[cfg(feature = "async_core")]
use core::future::Future;

use super::{
    CachePadded, DefaultShardHasher, Shard, ShardHasher, checked_per_shard_cap_from_total,
    checked_shard_count, default_shard_count_for_capacity, per_shard_cap_from_total, shard_index,
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
    /// Total logical capacity (sum of per-shard caps). Stored as `AtomicUsize` so
    /// [`set_max_size`](ShardedExpiringLruCache::set_max_size) can update it from `&self`.
    total_capacity: AtomicUsize,
}

/// A fully-concurrent, partitioned, LRU size-bounded in-memory cache with per-value expiry.
///
/// Each value controls its own expiration by implementing [`Expires`]. Expired entries
/// are checked on lookup and evicted on access or during explicit [`evict`](ConcurrentCacheEvict::evict) sweeps.
/// Eviction is also enforced independently per shard when capacity limits are hit.
///
/// Wraps an `Arc` — `clone()` is an Arc-share (shared state), not a deep copy.
/// Use [`deep_clone`](ShardedExpiringLruCache::deep_clone) to get an independent copy.
///
/// **Note**: `K` and `V` must implement `Clone` (`K` for LRU key tracking; `V` because reads
/// return owned values cloned from under the shard lock, in addition to `V: Expires`).
///
/// The shard-selection hasher `H` defaults to [`DefaultShardHasher`] (ahash-backed when the
/// `ahash` feature is enabled, otherwise `std::collections::hash_map::RandomState`), so
/// `ShardedExpiringLruCache<K, V>` names the common case. To use a custom [`ShardHasher`], call
/// [`ShardedExpiringLruCache::builder()`] and then
/// [`hasher`](ShardedExpiringLruCacheBuilder::hasher), which switches `H` to your hasher.
///
/// **Note**: Setting an `on_evict` callback requires the callback itself to be `'static` because
/// the cache stores it behind an `Arc<dyn Fn(&K, &V) + Send + Sync>`. This does not add `'static`
/// bounds to `K` or `V`.
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
pub struct ShardedExpiringLruCache<K, V, H = DefaultShardHasher> {
    inner: Arc<ExpiringLruInner<K, V, H>>,
}

impl<K, V, H> Clone for ShardedExpiringLruCache<K, V, H> {
    /// Arc-share clone — both handles point to the same underlying cache.
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<K, V, H> ShardedExpiringLruCache<K, V, H> {
    /// Sum of the per-shard counters for evictions **not** driven by LRU capacity pressure:
    /// expired entries dropped lazily on [`cache_get`](ConcurrentCached::cache_get) or swept by
    /// [`evict`](ShardedExpiringLruCache::evict) or [`retain`](Self::retain).
    ///
    /// These live in [`Shard::evictions`], one atomic per shard (like `hits`/`misses`), rather
    /// than in a single process-wide counter on `Arc<Inner>`: a thread bumping it has just held
    /// that shard's lock, so the line is already owned exclusively and no cross-core traffic is
    /// added. LRU **capacity** evictions (plus explicit removes, which are counted there for
    /// historical reasons) remain in each shard's inner `LruCache::evictions`;
    /// [`metrics`](Self::metrics) sums the two families.
    fn non_capacity_evictions(&self) -> u64 {
        self.inner
            .shards
            .iter()
            .map(|s| s.evictions.load(Ordering::Relaxed))
            .sum()
    }
}

impl<K, V, H> std::fmt::Debug for ShardedExpiringLruCache<K, V, H> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShardedExpiringLruCache")
            .field("shards", &self.inner.shards.len())
            .field(
                "capacity",
                &self.inner.total_capacity.load(Ordering::Relaxed),
            )
            // Same quantity as before the counter moved per-shard: the non-capacity
            // eviction total (LRU capacity evictions live in the inner stores).
            .field("evictions", &self.non_capacity_evictions())
            .finish_non_exhaustive()
    }
}

impl<K, V> ShardedExpiringLruCache<K, V, DefaultShardHasher>
where
    K: Hash + Eq + Clone,
    V: Expires,
{
    /// Construct a ready-to-use [`ShardedExpiringLruCache`] holding up to roughly `max_size`
    /// entries total, with the [`DefaultShardHasher`] and a default shard count.
    ///
    /// Note that the effective total capacity can still exceed `max_size` for small values
    /// because each shard reserves a minimum capacity (see
    /// [`max_size`](ShardedExpiringLruCacheBuilder::max_size)). The default shard count is now
    /// scaled down for small `max_size` (roughly `max_size / 16` shards, capped by the
    /// CPU-derived default), so the overshoot is modest -- `max_size = 100` yields 8 shards x 16
    /// = 128 effective capacity. For a custom hasher, shard count, per-shard cap, or `on_evict`,
    /// use [`builder`](Self::builder).
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
    /// the builder's hasher type and `build` then yields a `ShardedExpiringLruCache<K, V, H>`
    /// over that hasher. `new` and `builder` exist only on the default-hasher instantiation
    /// `ShardedExpiringLruCache<K, V, DefaultShardHasher>`, so a custom hasher is always
    /// introduced via `hasher`, never a `ShardedExpiringLruCache::<_, _, H>` turbofish.
    #[must_use]
    pub fn builder() -> ShardedExpiringLruCacheBuilder<K, V, DefaultShardHasher> {
        ShardedExpiringLruCacheBuilder::default()
    }
}

impl<K, V, H> ShardedExpiringLruCache<K, V, H>
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

    /// Route a borrowed key to the shard that owns the equivalent owned key.
    ///
    /// Calls the same [`ShardHasher::shard_hash`](super::ShardHasher::shard_hash) the owned path
    /// (`shard_of`) calls, only at `Q` instead of `K`, so the two cannot drift and a custom
    /// router's own `shard_hash` is honored here rather than bypassed. When `H` reaches
    /// `ShardHasher` through the blanket `BuildHasher` impl, both instantiations run
    /// [`routing_hash`](super::routing_hash), so the hash for `&Q` equals the hash for the owned
    /// `K` by the `Borrow` contract alone (equal keys hash equally), the same guarantee
    /// `HashMap::get(&str)` on a `String` key already relies on. When `H` carries hand-written
    /// impls, that agreement is the cross-impl contract documented on
    /// [`ShardHasher`](super::ShardHasher).
    #[inline]
    fn shard_of_borrowed<Q>(&self, k: &Q) -> &CachePadded<Shard<LruCache<K, V>>>
    where
        K: Borrow<Q>,
        Q: Hash + ?Sized,
        H: ShardHasher<Q>,
    {
        let h = self.inner.hasher.shard_hash(k);
        &self.inner.shards[shard_index(h, self.inner.shard_mask)]
    }
}

impl<K: Clone + Hash + Eq, V: Clone + Expires, H: ShardHasher<K>> ShardedExpiringLruCache<K, V, H> {
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
            inner: Arc::new(ExpiringLruInner {
                shards,
                shard_mask: self.inner.shard_mask,
                hasher: self.inner.hasher.clone(),
                on_evict: self.inner.on_evict.clone(),
                total_capacity: AtomicUsize::new(self.inner.total_capacity.load(Ordering::Relaxed)),
            }),
        }
    }
}

/// Shared lookup bodies. Routing is the caller's job (`shard_of` for an owned key,
/// `shard_of_borrowed` for a borrowed one), so the owned and borrowed entry points run one
/// implementation and cannot drift on metrics, eviction counts, or `on_evict`.
impl<K, V, H: ShardHasher<K>> ShardedExpiringLruCache<K, V, H>
where
    K: Hash + Eq + Clone,
    V: Clone + Expires,
{
    fn get_in<Q>(&self, shard: &CachePadded<Shard<LruCache<K, V>>>, k: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let mut guard = shard.lock.write();
        // The common case (a live hit) resolves in a SINGLE hash + probe: `get_if` promotes
        // LRU recency only when the predicate reports the value live, so an expired entry is
        // neither promoted nor removed here -- exactly the intent of the old peek-then-get
        // pair, at half the lookups. `track_hit_miss` is disabled on the inner `LruCache`, so
        // this probe touches no counter.
        let val = guard.get_if(k, |v| !v.is_expired()).cloned();
        if let Some(val) = val {
            drop(guard);
            shard.hits.fetch_add(1, Ordering::Relaxed);
            return Some(val);
        }

        // Not a live hit: either expired (still stored) or absent. `pop_raw` distinguishes
        // them -- `Some` means it was expired and is now removed, `None` means absent. This
        // costs the (rarer) miss path one extra probe to spare every hit one.
        let removed = guard.pop_raw(k);
        drop(guard);
        if let Some((ref key, ref val)) = removed {
            // `pop_raw` removes the entry without bumping the inner LRU eviction counter, so
            // track expired-on-access removals in the shard's non-capacity counter instead.
            // Explicit removes via `cache_remove` bump the inner LRU counter
            // (`guard.evictions`). Both feed `metrics().evictions` via its combined sum.
            shard.evictions.fetch_add(1, Ordering::Relaxed);
            if let Some(on_evict) = &self.inner.on_evict {
                on_evict(key, val);
            }
        }
        shard.misses.fetch_add(1, Ordering::Relaxed);
        None
    }

    /// Removes the entry and hands back the stored pair. Shared by `remove`, `remove_entry` and
    /// `delete`, which differ only in how they report it.
    ///
    /// Deliberately does not consult [`Expires::is_expired`](crate::Expires::is_expired):
    /// `remove_entry` and `delete` report
    /// the stored pair whether or not it had expired, and `is_expired` is arbitrary user code
    /// with no purity guarantee. Of `pop_in`'s two callers, only `remove_in` consults it;
    /// `delete` reaches `pop_in` indirectly, through `remove_entry_in`.
    fn pop_in<Q>(&self, shard: &CachePadded<Shard<LruCache<K, V>>>, k: &Q) -> Option<(K, V)>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let removed = {
            let mut guard = shard.lock.write();
            let removed = guard.pop_raw(k);
            if removed.is_some() {
                guard.evictions.fetch_add(1, Ordering::Relaxed);
            }
            removed
        };
        let (key, val) = removed?;
        if let Some(on_evict) = &self.inner.on_evict {
            on_evict(&key, &val);
        }
        Some((key, val))
    }

    fn remove_in<Q>(&self, shard: &CachePadded<Shard<LruCache<K, V>>>, k: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.pop_in(shard, k)
            .and_then(|(_, val)| (!val.is_expired()).then_some(val))
    }

    fn remove_entry_in<Q>(
        &self,
        shard: &CachePadded<Shard<LruCache<K, V>>>,
        k: &Q,
    ) -> Option<(K, V)>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.pop_in(shard, k)
    }

    fn contains_in<Q>(&self, shard: &CachePadded<Shard<LruCache<K, V>>>, k: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        shard
            .lock
            .read()
            .cache_peek(k)
            .is_some_and(|v| !v.is_expired())
    }

    fn peek_in<Q>(&self, shard: &CachePadded<Shard<LruCache<K, V>>>, k: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let guard = shard.lock.read();
        guard.cache_peek(k).filter(|v| !v.is_expired()).cloned()
    }
}

impl<K, V, H: ShardHasher<K>> ShardedExpiringLruCache<K, V, H>
where
    K: Hash + Eq + Clone,
    V: Clone + Expires,
{
    /// Retrieve a cached value, returning `None` on a miss or if the entry has expired.
    ///
    /// This is the infallible ergonomic API for the concrete type. Generic code over
    /// [`ConcurrentCached`] should use the `Result`-returning trait methods (`cache_get` or the
    /// `get` alias from [`ConcurrentCachedExt`](crate::ConcurrentCachedExt)), callable as
    /// `ConcurrentCachedExt::get(&store, k)` when this inherent method is in scope.
    ///
    /// Takes any borrowed form of the key, so a `String`-keyed cache reads with a `&str`:
    ///
    /// ```rust
    /// use cached::{Expires, ShardedExpiringLruCache};
    ///
    /// #[derive(Clone, PartialEq, Debug)]
    /// struct Session(u32);
    /// impl Expires for Session {
    ///     fn is_expired(&self) -> bool {
    ///         false
    ///     }
    /// }
    ///
    /// let cache: ShardedExpiringLruCache<String, Session> = ShardedExpiringLruCache::new(64);
    /// cache.set("a".to_string(), Session(1));
    /// assert_eq!(cache.get("a"), Some(Session(1)));
    /// ```
    ///
    /// This method is bounded on `H: ShardHasher<Q>`. Every hasher that reaches [`ShardHasher`]
    /// through the blanket `BuildHasher` impl satisfies it for every `Q: Hash`, including the
    /// default [`DefaultShardHasher`], so the owned-key call `cache.get(&k)` and the borrowed
    /// `cache.get("k")` both work. A store built on a hand-written `ShardHasher` keeps this
    /// method at the key types it implements: `ShardHasher<K>` alone gives the owned-key call,
    /// and a borrowed form needs a second `impl ShardHasher<Q>` on the router, which must agree
    /// with the first (see [`ShardHasher`]'s consistency contract).
    ///
    /// `K` and `Q` must hash identically, which is what the `Borrow` contract already requires. A
    /// borrowed form that hashes differently routes to a different shard, so the lookup misses an
    /// entry that is present and counts the miss against it. Pass the borrowed key directly:
    /// `cache.get(&k)` where `k: &String` (the loop variable of `for k in &keys`, for instance)
    /// infers `Q = &String` and fails on `String: Borrow<&String>`, so drop the extra `&` and
    /// write `cache.get(k)`. A `&Box<K>` or `&Arc<K>` has no extra `&` to drop and needs an
    /// explicit deref instead (`cache.get(&**k)`). With a single-impl router the failure is an
    /// argument type mismatch (E0308, `expected &UserId, found &u64`), not a missing bound.
    #[must_use]
    pub fn get<Q>(&self, k: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
        H: ShardHasher<Q>,
    {
        self.get_in(self.shard_of_borrowed(k), k)
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
    /// This is the infallible ergonomic API for the concrete type. Takes any borrowed form of
    /// the key; see [`get`](Self::get) for the `H: ShardHasher<Q>` bound that carries.
    pub fn remove<Q>(&self, k: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
        H: ShardHasher<Q>,
    {
        self.remove_in(self.shard_of_borrowed(k), k)
    }

    /// Remove a cached entry and return the stored key and value, if present.
    ///
    /// This is the infallible ergonomic API for the concrete type. Takes any borrowed form of
    /// the key; see [`get`](Self::get) for the `H: ShardHasher<Q>` bound that carries.
    pub fn remove_entry<Q>(&self, k: &Q) -> Option<(K, V)>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
        H: ShardHasher<Q>,
    {
        self.remove_entry_in(self.shard_of_borrowed(k), k)
    }

    /// Delete a cached entry without returning the value. Returns `true` if an entry was removed.
    ///
    /// This is the infallible ergonomic API for the concrete type. Takes any borrowed form of
    /// the key; see [`get`](Self::get) for the `H: ShardHasher<Q>` bound that carries.
    pub fn delete<Q>(&self, k: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
        H: ShardHasher<Q>,
    {
        self.remove_entry_in(self.shard_of_borrowed(k), k).is_some()
    }

    /// Remove all entries from every shard and reset metrics.
    ///
    /// This is the infallible ergonomic API for the concrete type.
    pub fn reset(&self) {
        ConcurrentCached::cache_reset(self).unwrap()
    }

    /// Return true if a live (not expired) value is stored for `k`. Peek-based: no recency update, no hit/miss metrics.
    ///
    /// Takes any borrowed form of the key; see [`get`](Self::get) for the
    /// `H: ShardHasher<Q>` bound that carries.
    #[must_use]
    pub fn contains<Q>(&self, k: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
        H: ShardHasher<Q>,
    {
        self.contains_in(self.shard_of_borrowed(k), k)
    }

    /// Return a clone of the live (not expired) value stored for `k` without
    /// observable side effects: no LRU recency update, no hit/miss metrics, no lazy
    /// removal of an expired entry. The single-owner counterpart is
    /// [`CachedPeek::cache_peek`](crate::CachedPeek::cache_peek); the sharded stores
    /// return a clone rather than a reference because the value lives behind a
    /// per-shard lock.
    ///
    /// Takes any borrowed form of the key; see [`get`](Self::get) for the
    /// `H: ShardHasher<Q>` bound that carries.
    #[must_use]
    pub fn peek<Q>(&self, k: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
        H: ShardHasher<Q>,
    {
        self.peek_in(self.shard_of_borrowed(k), k)
    }
}

impl<K, V, H: ShardHasher<K>> ShardedExpiringLruCache<K, V, H>
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
        let mut non_capacity_evictions = 0u64;
        let mut size = 0usize;
        for shard in self.inner.shards.iter() {
            hits += shard.hits.load(Ordering::Relaxed);
            misses += shard.misses.load(Ordering::Relaxed);
            // Per-shard non-capacity evictions (lazy expiry / evict / retain); the
            // inner `LruCache` counter below holds this shard's capacity evictions and its
            // explicit removes. The two families are disjoint, so summing cannot double-count.
            non_capacity_evictions += shard.evictions.load(Ordering::Relaxed);
            let guard = shard.lock.read();
            if let Some(e) = guard.cache_evictions() {
                inner_evictions += e;
            }
            size += guard.cache_size();
        }
        CacheMetrics {
            hits: Some(hits),
            misses: Some(misses),
            evictions: Some(inner_evictions + non_capacity_evictions),
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

    /// Remove every entry that is expired (per [`Expires::is_expired`]) **or** for which `keep`
    /// returns `false` — expired entries are removed regardless of `keep`, matching
    /// [`ExpiringLruCache::retain`](crate::ExpiringLruCache::retain) semantics. `on_evict` fires
    /// (if configured) and `metrics().evictions` increments once per removed entry (via the
    /// per-shard eviction counter (`Shard::evictions`), the same one used by
    /// [`evict`](Self::evict)). The LRU recency order of the surviving entries in each shard is
    /// unchanged.
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
        let mut total_removed = 0usize;
        for shard in self.inner.shards.iter() {
            let removed: Vec<(K, V)> = {
                let mut guard = shard.lock.write();
                let doomed: Vec<K> = guard
                    .iter()
                    .filter_map(|(k, v)| {
                        if v.is_expired() || !keep(k, v) {
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
                    for (k, v) in &removed {
                        on_evict(k, v);
                    }
                }
            }
        }
        total_removed
    }

    /// Effective total capacity across all shards.
    ///
    /// When constructed with [`max_size`](ShardedExpiringLruCacheBuilder::max_size), this may
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
    /// [`ExpiringLruCache::set_max_size`](crate::ExpiringLruCache::set_max_size) signature.
    ///
    /// Takes `&self`: shards use interior mutability (per-shard write locks), so
    /// the method is callable through `Arc` or any shared reference — no external
    /// lock is needed, unlike the `&mut self` single-owner counterpart.
    ///
    /// The new per-shard capacity is recomputed using the same policy the builder
    /// uses for [`max_size`](ShardedExpiringLruCacheBuilder::max_size): ceiling division
    /// across shards with a minimum of 16 entries per shard when `shards > 1`.
    /// After resizing, any configuration previously set via
    /// [`per_shard_max_size`](ShardedExpiringLruCacheBuilder::per_shard_max_size) is replaced
    /// by the total-based policy.
    ///
    /// On shrink, excess LRU entries are evicted per shard: `on_evict` fires for
    /// each evicted entry and the eviction counter is incremented accordingly.
    /// The shrink evicts strictly by LRU recency and ignores expiry state — an
    /// expired but recently-used entry survives while a live but
    /// least-recently-used entry is evicted. Call [`evict`](Self::evict) first
    /// to sweep expired entries if they should be dropped preferentially.
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
    /// [`try_set_max_size`](ShardedExpiringLruCache::try_set_max_size) to avoid either panic.
    ///
    /// # See also
    ///
    /// [`ShardedLruCache::set_max_size`](crate::ShardedLruCache::set_max_size) and
    /// [`ShardedLruTtlCache::set_max_size`](crate::ShardedLruTtlCache::set_max_size)
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

    /// Fallible counterpart of [`set_max_size`](ShardedExpiringLruCache::set_max_size):
    /// validates that `max_size` is non-zero and then delegates to `set_max_size`.
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

impl<K, V, H: ShardHasher<K>> crate::ConcurrentCacheSetMaxSize for ShardedExpiringLruCache<K, V, H>
where
    K: Hash + Eq + Clone,
    V: Expires,
{
    fn set_max_size(&self, max_size: usize) -> Option<usize> {
        ShardedExpiringLruCache::set_max_size(self, max_size)
    }

    fn try_set_max_size(&self, max_size: usize) -> Result<Option<usize>, crate::SetMaxSizeError> {
        ShardedExpiringLruCache::try_set_max_size(self, max_size)
    }
}

impl<K, V, H: ShardHasher<K>> crate::ConcurrentCacheClearWithOnEvict
    for ShardedExpiringLruCache<K, V, H>
where
    K: Hash + Eq + Clone,
    V: Expires,
{
    fn cache_clear_with_on_evict(&self) {
        ShardedExpiringLruCache::cache_clear_with_on_evict(self);
    }
}

impl<K, V, H> ConcurrentCacheBase for ShardedExpiringLruCache<K, V, H>
where
    K: Hash + Eq + Clone,
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

    fn cache_capacity(&self) -> Option<usize> {
        // Acquire: see `capacity()`.
        Some(self.inner.total_capacity.load(Ordering::Acquire))
    }

    fn cache_evictions(&self) -> Option<u64> {
        let mut inner_evictions = 0u64;
        for shard in self.inner.shards.iter() {
            let guard = shard.lock.read();
            if let Some(e) = Cached::cache_evictions(&*guard) {
                inner_evictions += e;
            }
        }
        Some(inner_evictions + self.non_capacity_evictions())
    }
}

impl<K, V, H> ConcurrentCached<K, V> for ShardedExpiringLruCache<K, V, H>
where
    K: Hash + Eq + Clone,
    V: Clone + Expires,
    H: ShardHasher<K>,
{
    fn cache_get(&self, k: &K) -> Result<Option<V>, Self::Error> {
        Ok(self.get_in(self.shard_of(k), k))
    }

    fn cache_set(&self, k: K, v: V) -> Result<Option<V>, Self::Error> {
        let shard = self.shard_of(&k);
        // With a callback we need the *stored* key to hand to it, so the write goes through
        // `cache_set_returning_entry`; otherwise a plain set. Both promote an overwritten key to
        // MRU, so the two branches agree on eviction order. A displaced
        // expired value is counted as an eviction under the lock (matching cache_remove) and
        // filtered from the return; a live displaced value is returned to the caller unchanged.
        // `is_expired()` is evaluated exactly once, while the write lock is still held, and the
        // result carried through the tuple (matching the other sharded expiring stores): a value
        // crossing the expiry threshold between two evaluations would otherwise fire `on_evict`
        // without counting the eviction.
        let old: Option<(Option<K>, V, bool)> = {
            let mut guard = shard.lock.write();
            let old = if self.inner.on_evict.is_some() {
                guard.cache_set_returning_entry(k, v).map(|(ok, ov)| {
                    let expired = ov.is_expired();
                    (Some(ok), ov, expired)
                })
            } else {
                guard.cache_set(k, v).map(|ov| {
                    let expired = ov.is_expired();
                    (None, ov, expired)
                })
            };
            if matches!(&old, Some((_, _, true))) {
                // `guard.evictions` is the inner LRU counter (unlike expired-on-access removals
                // in `cache_get`, which use the shard's non-capacity counter because `pop_raw`
                // bypasses the inner one). Both feed the combined sum in `metrics()`.
                guard.evictions.fetch_add(1, Ordering::Relaxed);
            }
            old
        };
        match old {
            Some((key, ov, true)) => {
                if let (Some(on_evict), Some(key)) = (&self.inner.on_evict, &key) {
                    on_evict(key, &ov);
                }
                Ok(None)
            }
            Some((_, ov, false)) => Ok(Some(ov)),
            None => Ok(None),
        }
    }

    /// Removes the entry and returns the value only if it is still live;
    /// an expired value is removed but reported as `Ok(None)`. Use
    /// [`cache_remove_entry`](ConcurrentCached::cache_remove_entry) to
    /// receive the value regardless of expiry.
    fn cache_remove(&self, k: &K) -> Result<Option<V>, Self::Error> {
        Ok(self.remove_in(self.shard_of(k), k))
    }

    /// Removes the entry and returns it **regardless of expiry** (unlike
    /// [`cache_remove`](ConcurrentCached::cache_remove), which filters
    /// expired values).
    fn cache_remove_entry(&self, k: &K) -> Result<Option<(K, V)>, Self::Error> {
        Ok(self.remove_entry_in(self.shard_of(k), k))
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
            // The shard's non-capacity eviction counter (lazy expiry / evict / retain / clear).
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
        Ok(self.contains_in(self.shard_of(k), k))
    }
}

impl<K, V, H> ConcurrentCachePeek<K, V> for ShardedExpiringLruCache<K, V, H>
where
    K: Hash + Eq + Clone,
    V: Clone + Expires,
    H: ShardHasher<K>,
{
    fn cache_peek(&self, k: &K) -> Result<Option<V>, Self::Error> {
        Ok(self.peek_in(self.shard_of(k), k))
    }
}

#[cfg(feature = "async_core")]
#[cfg_attr(docsrs, doc(cfg(feature = "async_core")))]
impl<K, V, H> ConcurrentCachePeekAsync<K, V> for ShardedExpiringLruCache<K, V, H>
where
    K: Hash + Eq + Clone + Send + Sync,
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
impl<K, V, H> ConcurrentCachedAsync<K, V> for ShardedExpiringLruCache<K, V, H>
where
    K: Hash + Eq + Clone + Send + Sync,
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

impl<K, V, H> ShardedExpiringLruCache<K, V, H>
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
}

impl<K, V, H> ConcurrentCacheEvict for ShardedExpiringLruCache<K, V, H>
where
    K: Clone + Hash + Eq,
    V: Expires,
    H: ShardHasher<K>,
{
    fn evict(&self) -> usize {
        ShardedExpiringLruCache::evict(self)
    }
}

/// Builder for [`ShardedExpiringLruCache`].
///
/// Note: there is intentionally **no `.ttl()` setter**. A sharded expiring LRU cache has no
/// global expiry duration — each value decides when it is expired via the [`Expires`] trait,
/// while `max_size` bounds the entry count via LRU. For a single global TTL applied to every
/// entry, use [`ShardedLruTtlCache`](crate::ShardedLruTtlCache) instead.
#[doc(alias = "ttl")]
pub struct ShardedExpiringLruCacheBuilder<K, V, H = DefaultShardHasher> {
    shards: Option<usize>,
    max_size: Option<usize>,
    per_shard_max_size: Option<usize>,
    hasher: Option<H>,
    on_evict: Option<OnEvict<K, V>>,
    _k: std::marker::PhantomData<K>,
    _v: std::marker::PhantomData<V>,
}

impl<K, V> Default for ShardedExpiringLruCacheBuilder<K, V, DefaultShardHasher> {
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

impl<K, V> ShardedExpiringLruCacheBuilder<K, V> {
    /// Create a builder with default settings. Equivalent to [`ShardedExpiringLruCache::builder`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl<K, V, H> ShardedExpiringLruCacheBuilder<K, V, H> {
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
    /// small values. On the default shard-count path the shard count itself is scaled down for
    /// small `max_size` (roughly `max_size / 16`, capped by the CPU-derived default), so the
    /// overshoot stays modest: `max_size = 100` builds 8 shards x 16 = 128 effective capacity.
    /// An explicit [`shards`](Self::shards) count opts out of that scaling, so a large explicit
    /// count with a small `max_size` still overshoots by `shards * 16` (e.g. `max_size = 10`
    /// with an explicit 8 shards yields 128).
    /// [`metrics()`](ShardedExpiringLruCache::metrics)'s `capacity` and `entry_count` reflect
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

    /// Set a callback invoked when an entry is evicted. Fires in six situations:
    /// for LRU capacity evictions; expired-entry removal during
    /// [`cache_get`](ConcurrentCached::cache_get); explicitly via
    /// [`evict`](ShardedExpiringLruCache::evict); on explicit
    /// [`cache_remove`](ConcurrentCached::cache_remove); on
    /// [`cache_remove_entry`](ConcurrentCached::cache_remove_entry); and on
    /// [`cache_set`](ConcurrentCached::cache_set) when the displaced entry is already expired.
    /// Does **not** fire on [`clear`](ShardedExpiringLruCache::clear);
    /// use [`cache_clear_with_on_evict`](ShardedExpiringLruCache::cache_clear_with_on_evict) to opt in.
    /// [`cache_clear_with_on_evict`](ShardedExpiringLruCache::cache_clear_with_on_evict) fires
    /// callbacks after releasing the shard lock.
    ///
    /// Capacity-eviction callbacks run while the affected shard's write lock is held. Do not call
    /// methods on the same sharded cache from the callback; doing so can deadlock if the callback
    /// re-enters the locked shard. Expiry sweeps via [`evict`](ShardedExpiringLruCache::evict)
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
    /// # Errors
    ///
    /// Returns [`Err(BuildError)`](crate::stores::BuildError) if the builder
    /// configuration is invalid (the same conditions as [`build`](Self::build)):
    /// `max_size` / `per_shard_max_size` not set or is `0`, or both set simultaneously.
    #[must_use = "the Result from copy_from() must be used"]
    pub fn copy_from<H2: ShardHasher<K>>(
        self,
        existing: &ShardedExpiringLruCache<K, V, H2>,
    ) -> Result<ShardedExpiringLruCache<K, V, H>, BuildError>
    where
        K: Clone + Hash + Eq,
        V: Clone + Expires,
        H: ShardHasher<K>,
    {
        let new_cache = self.build()?;
        for shard in existing.inner.shards.iter() {
            // iter_order returns MRU-first; insert in reverse (LRU-first) so
            // that MRU entries land at the head of the new cache.
            let entries: Vec<(K, V)> = {
                let guard = shard.lock.read();
                guard.iter_order_raw()
            };
            for (k, v) in entries.into_iter().rev() {
                if !v.is_expired() {
                    let _ = ConcurrentCached::cache_set(&new_cache, k, v);
                }
            }
        }
        Ok(new_cache)
    }

    /// Build the cache, returning an error if required fields are missing or invalid.
    ///
    /// Use [`ShardedExpiringLruCache::builder()`] to obtain a builder, set at least
    /// [`max_size`](Self::max_size), then call `.build()`.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError`] if `size` (or `per_shard_max_size`) was not set, is `0`,
    /// or if both `max_size` and `per_shard_max_size` are set simultaneously,
    /// or if the shard count overflows.
    #[must_use = "the Result from build() must be used"]
    pub fn build(self) -> Result<ShardedExpiringLruCache<K, V, H>, BuildError>
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
        Ok(ShardedExpiringLruCache {
            inner: Arc::new(ExpiringLruInner {
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
}

impl<K, V, H> ConcurrentCloneCached<K, V> for ShardedExpiringLruCache<K, V, H>
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

impl<K, V, H> ConcurrentCacheExpiry<K, V> for ShardedExpiringLruCache<K, V, H>
where
    K: Hash + Eq + Clone,
    V: Expires,
    H: ShardHasher<K>,
{
    /// Returns the stored value and its expiry instant, with no read side effects.
    ///
    /// Takes only a read lock and does not promote LRU recency. The instant comes from
    /// [`Expires::expires_at`], which is **advisory only** on this store, not authoritative: the
    /// default implementation returns `None`, so it is `None` for any value type that does not
    /// override it (including entries that are expired), and it may be in the past for an entry
    /// [`Expires::is_expired`] reports as live. `cache_peek_with_expiry_status` (on
    /// [`ConcurrentCloneCached`]), which consults `is_expired`, remains the authoritative
    /// liveness read here; see the [`ConcurrentCacheExpiry`] trait docs for the caveat in full.
    /// An expired entry is returned and is **not** removed; the hits/misses counters and the
    /// LRU order are untouched.
    fn cache_peek_expires_at(&self, k: &K) -> (Option<V>, Option<crate::time::Instant>)
    where
        V: Clone,
    {
        let shard = self.shard_of(k);
        let guard = shard.lock.read();
        match guard.cache_peek(k) {
            None => (None, None),
            Some(v) => (Some(v.clone()), v.expires_at()),
        }
    }

    /// Returns whether the key is present and its expiry instant, without the value.
    ///
    /// The value-free counterpart of
    /// [`cache_peek_expires_at`](ConcurrentCacheExpiry::cache_peek_expires_at): the same
    /// non-promoting read lock and the same advisory instant, with no clone and no `V: Clone`
    /// bound.
    ///
    /// The presence flag is **not** advisory: it reports whether an entry is stored for `k`
    /// regardless of [`Expires::is_expired`]. The deadline is advisory in the same way as
    /// `cache_peek_expires_at`'s: `None` for any value type that does not override
    /// [`Expires::expires_at`] (including an expired entry), so `(true, None)` here means
    /// "present, deadline unknown", never "present and live". `(false, None)` when the key is
    /// absent. An expired entry is reported and is **not** removed; the hits/misses counters and
    /// the LRU order are untouched.
    fn cache_expires_at(&self, k: &K) -> (bool, Option<crate::time::Instant>) {
        let shard = self.shard_of(k);
        let guard = shard.lock.read();
        match guard.cache_peek(k) {
            None => (false, None),
            Some(v) => (true, v.expires_at()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ConcurrentCached;
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
    fn default_shard_count_scales_with_max_size() {
        use crate::stores::sharded::{default_shard_count, default_shard_count_for_capacity};
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .max_size(100)
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
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .shards(64)
            .max_size(100)
            .build()
            .unwrap();
        assert_eq!(c.shards(), 64);
        assert_eq!(c.capacity(), 64 * 16);
    }

    #[test]
    fn default_shard_count_clamps_at_upper_bound_end_to_end() {
        // This builder has its own resolve_shard_count copy; verify the large-max_size clamp
        // reaches default_shard_count() through it, with the expectation computed at runtime.
        use crate::stores::sharded::default_shard_count;
        let d = default_shard_count();
        let big = d.checked_mul(16).unwrap().checked_mul(4).unwrap();
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .max_size(big)
            .build()
            .unwrap();
        assert_eq!(c.shards(), d);
    }

    #[test]
    fn per_shard_max_size_keeps_plain_default_shard_count() {
        use crate::stores::sharded::default_shard_count;
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .per_shard_max_size(4)
            .build()
            .unwrap();
        assert_eq!(c.shards(), default_shard_count());
    }

    #[test]
    fn cache_set_over_expired_returns_none_fires_on_evict_and_counts() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU64, Ordering as AOrd};
        let count = Arc::new(AtomicU64::new(0));
        let count2 = count.clone();
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .shards(1)
            .max_size(4)
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
    fn new_returns_ready_cache_respecting_max_size() {
        // shards(1) gives an exact eviction bound.
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .shards(1)
            .max_size(2)
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(
            &c,
            1,
            Val {
                v: 10,
                expired: false,
            },
        )
        .unwrap();
        assert_eq!(
            SyncConcurrentCached::cache_get(&c, &1)
                .unwrap()
                .map(|v| v.v),
            Some(10)
        );
        SyncConcurrentCached::cache_set(
            &c,
            2,
            Val {
                v: 20,
                expired: false,
            },
        )
        .unwrap();
        SyncConcurrentCached::cache_set(
            &c,
            3,
            Val {
                v: 30,
                expired: false,
            },
        )
        .unwrap(); // evicts LRU (1)
        assert_eq!(c.len(), 2);
        assert!(SyncConcurrentCached::cache_get(&c, &1).unwrap().is_none());

        // Inherent `new` returns a ready cache too.
        let c2 = ShardedExpiringLruCache::<u32, Val>::new(64);
        SyncConcurrentCached::cache_set(
            &c2,
            1,
            Val {
                v: 1,
                expired: false,
            },
        )
        .unwrap();
        assert_eq!(
            SyncConcurrentCached::cache_get(&c2, &1)
                .unwrap()
                .map(|v| v.v),
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
        let new_cache = ShardedExpiringLruCache::<u32, Val>::builder()
            .max_size(64)
            .copy_from(&old)
            .unwrap();
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
        let new_cache = ShardedExpiringLruCache::<u32, Val>::builder()
            .max_size(64)
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
        let new_cache = ShardedExpiringLruCache::<u32, Val>::builder()
            .shards(1)
            .max_size(8)
            .copy_from(&old)
            .unwrap();
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
        let cache = ShardedExpiringLruCache::<u32, Val>::builder()
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
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
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
    fn cache_clear_with_on_evict_counts_evictions_without_callback() {
        // metrics().evictions must not depend on an on_evict observer being attached.
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .shards(1)
            .max_size(64)
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
    fn clear_does_not_fire_on_evict() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU64, Ordering as AtomicOrd};
        let count = Arc::new(AtomicU64::new(0));
        let count2 = count.clone();
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
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
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
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
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
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
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
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
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
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
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
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
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
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

    // --- ConcurrentCacheExpiry tests ---

    #[test]
    fn peek_expires_at_absent_key_returns_none_none() {
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .max_size(64)
            .build()
            .unwrap();
        let (value, expires_at) = ConcurrentCacheExpiry::cache_peek_expires_at(&c, &1);
        assert!(value.is_none());
        assert_eq!(expires_at, None);
        let (value, expires_at) = ConcurrentCacheExpiry::peek_expires_at(&c, &1);
        assert!(value.is_none());
        assert_eq!(expires_at, None);
    }

    #[test]
    fn peek_expires_at_live_entry_returns_the_stored_future_deadline() {
        let c = ShardedExpiringLruCache::<u32, TimedVal>::builder()
            .max_size(64)
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
    fn peek_expires_at_value_not_overriding_expires_at_returns_no_deadline() {
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .max_size(64)
            .build()
            .unwrap();
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
            "a value type that does not override expires_at must report no deadline"
        );
        // Distinguishable from an absent key by the value, not by the deadline.
        let (val, expired) = ConcurrentCloneCached::cache_peek_with_expiry_status(&c, &1u32);
        assert_eq!(val.map(|x| x.v), Some(10));
        assert!(!expired);
    }

    #[test]
    fn peek_expires_at_expired_entry_returns_a_past_deadline_and_keeps_the_entry() {
        let c = ShardedExpiringLruCache::<u32, TimedVal>::builder()
            .max_size(64)
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
        let (value2, expired2) = ConcurrentCloneCached::cache_peek_with_expiry_status(&c, &1u32);
        assert_eq!(value2.map(|x| x.v), Some(10));
        assert!(
            expired2,
            "the entry must still report expired via is_expired"
        );
    }

    #[test]
    fn peek_expires_at_does_not_touch_hit_or_miss_counters() {
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .max_size(64)
            .build()
            .unwrap();
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
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .max_size(64)
            .build()
            .unwrap();
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
    fn peek_expires_at_does_not_promote_lru() {
        // max_size(2) + shards(1): a single shard with 2 slots. If the peek promoted
        // recency, inserting a third entry would evict key 2 (MRU before peek); if it does
        // not promote, key 1 remains LRU and is evicted instead.
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .max_size(2)
            .shards(1)
            .build()
            .unwrap();

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

        // Peek key 1 -- must NOT promote it to MRU.
        let (value, expires_at) = ConcurrentCacheExpiry::cache_peek_expires_at(&c, &1u32);
        assert_eq!(value.map(|x| x.v), Some(10));
        assert_eq!(expires_at, None);

        // Inserting key 3 must evict key 1 (still LRU because the peek did not promote it).
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
    fn peek_expires_at_multi_shard() {
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .max_size(256)
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
    // file. Mirrors the single-owner `ExpiringLruCache`'s
    // `peek_expires_at_advisory_past_deadline_survives_while_is_expired_reports_live`.
    #[test]
    fn peek_expires_at_advisory_past_deadline_survives_while_is_expired_reports_live() {
        let past = crate::time::Instant::now() - std::time::Duration::from_secs(3600);
        let c = ShardedExpiringLruCache::<u32, TimedVal>::builder()
            .max_size(64)
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

    // Certification gap (L5): the test above pins only the past-deadline-while-live
    // direction of the bidirectional advisory caveat. Port the other direction: a
    // value type whose `expires_at()` is in the FUTURE while `is_expired()` reports
    // EXPIRED must still return that future deadline, and a real `cache_get` must
    // still obey `is_expired` and treat the entry as gone. Mirrors the single-owner
    // `ExpiringLruCache`'s
    // `peek_expires_at_advisory_future_deadline_survives_while_is_expired_reports_expired`.
    #[test]
    fn peek_expires_at_advisory_future_deadline_survives_while_is_expired_reports_expired() {
        let future = crate::time::Instant::now() + std::time::Duration::from_secs(3600);
        let c = ShardedExpiringLruCache::<u32, TimedVal>::builder()
            .max_size(64)
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(
            &c,
            1u32,
            TimedVal {
                v: 10,
                expired: true,    // is_expired() reports EXPIRED
                deadline: future, // yet the advisory deadline is still in the future
            },
        )
        .expect("insert must succeed");

        let (value, expires_at) = ConcurrentCacheExpiry::cache_peek_expires_at(&c, &1u32);
        assert_eq!(value.map(|x| x.v), Some(10));
        assert_eq!(
            expires_at,
            Some(future),
            "the advisory deadline must be surfaced unchanged, even though it is in the future"
        );
        let (val, expired) = ConcurrentCloneCached::cache_peek_with_expiry_status(&c, &1u32);
        assert_eq!(val.map(|x| x.v), Some(10));
        assert!(
            expired,
            "cache_peek_with_expiry_status must still report the entry as expired"
        );
        assert_eq!(c.len(), 1, "the peek must not remove the entry");

        // The store's real read path must obey is_expired, not the advisory deadline.
        assert!(
            SyncConcurrentCached::cache_get(&c, &1u32)
                .expect("cache_get must succeed")
                .is_none(),
            "is_expired remains the authority the store acts on, regardless of a \
             future-looking advisory deadline"
        );
        assert_eq!(
            c.len(),
            0,
            "the expired entry must be swept on the real access"
        );
    }

    #[test]
    fn peek_expires_at_reports_absent_after_evict_removes_the_entry() {
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .max_size(64)
            .build()
            .unwrap();
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
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .max_size(64)
            .build()
            .unwrap();
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

        let c = ShardedExpiringLruCache::<u32, TimedVal>::builder()
            .max_size(64)
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
        let c2 = ShardedExpiringLruCache::<u32, Val>::builder()
            .max_size(64)
            .build()
            .unwrap();
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

        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .max_size(64)
            .build()
            .unwrap();
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
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .max_size(256)
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
        let c = ShardedExpiringLruCache::<u32, TimedVal>::builder()
            .max_size(64)
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

    // --- ConcurrentCacheExpiry::cache_expires_at (the value-free read) ---

    #[test]
    fn expires_at_absent_key_returns_false_none() {
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .max_size(64)
            .build()
            .unwrap();
        assert_eq!(
            ConcurrentCacheExpiry::cache_expires_at(&c, &1u32),
            (false, None)
        );
        assert_eq!(ConcurrentCacheExpiry::expires_at(&c, &1u32), (false, None));
    }

    #[test]
    fn expires_at_live_entry_returns_the_stored_future_deadline() {
        let c = ShardedExpiringLruCache::<u32, TimedVal>::builder()
            .max_size(64)
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

        let (present, expires_at) = ConcurrentCacheExpiry::cache_expires_at(&c, &1u32);
        assert!(present, "a stored key must report present");
        assert_eq!(
            expires_at,
            Some(deadline),
            "the reported deadline must be the one the value reports"
        );
    }

    #[test]
    fn expires_at_value_not_overriding_expires_at_reports_present_with_no_deadline() {
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .max_size(64)
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(
            &c,
            1u32,
            Val {
                v: 10,
                expired: false,
            },
        )
        .expect("insert must succeed");
        assert_eq!(
            ConcurrentCacheExpiry::cache_expires_at(&c, &1u32),
            (true, None),
            "a value type that does not override expires_at reports present with no deadline"
        );
        assert_eq!(
            ConcurrentCacheExpiry::cache_expires_at(&c, &2u32),
            (false, None),
            "absent must remain distinguishable from present-with-no-deadline"
        );
    }

    // Pins the documented Expires-store caveat for the value-free read too: `(true, None)` does
    // not mean live. `Val` does not override `expires_at`, so an EXPIRED `Val` still reports
    // `(true, None)`, not `(true, Some(past))` -- presence is independent of expiry, and the
    // advisory `None` is not evidence of liveness.
    #[test]
    fn expires_at_expired_value_without_expires_at_override_reports_present_with_no_deadline() {
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .max_size(64)
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(
            &c,
            1u32,
            Val {
                v: 10,
                expired: true,
            },
        )
        .expect("insert must succeed");

        assert_eq!(
            ConcurrentCacheExpiry::cache_expires_at(&c, &1u32),
            (true, None),
            "an expired entry is still reported present; None is not evidence of liveness"
        );
        // is_expired remains the authoritative liveness read.
        let (_, expired) = ConcurrentCloneCached::cache_peek_with_expiry_status(&c, &1u32);
        assert!(
            expired,
            "is_expired is authoritative, unlike the advisory expires_at"
        );
    }

    #[test]
    fn expires_at_expired_entry_returns_a_past_deadline_and_keeps_the_entry() {
        let c = ShardedExpiringLruCache::<u32, TimedVal>::builder()
            .max_size(64)
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
        let (present, expires_at) = ConcurrentCacheExpiry::cache_expires_at(&c, &1u32);
        assert!(present, "an expired entry is still reported present");
        assert_eq!(expires_at, Some(deadline), "the past deadline is reported");

        // Not removed by the read, and no eviction counted.
        let after = c.metrics();
        assert_eq!(after.evictions, before.evictions);
        assert_eq!(c.len(), 1);
    }

    // The two reads must never disagree: same deadline, and the presence flag must track
    // whether the value-bearing read returned `Some`.
    #[test]
    fn expires_at_agrees_with_peek_expires_at_across_all_return_shapes() {
        let c = ShardedExpiringLruCache::<u32, TimedVal>::builder()
            .max_size(64)
            .shards(4)
            .build()
            .unwrap();

        let check = |k: u32, label: &str| {
            let (value, peeked) = ConcurrentCacheExpiry::cache_peek_expires_at(&c, &k);
            let (present, deadline) = ConcurrentCacheExpiry::cache_expires_at(&c, &k);
            assert_eq!(
                present,
                value.is_some(),
                "presence flag disagrees ({label})"
            );
            assert_eq!(deadline, peeked, "deadline disagrees ({label})");
            assert_eq!(
                ConcurrentCacheExpiry::expires_at(&c, &k),
                ConcurrentCacheExpiry::cache_expires_at(&c, &k),
                "alias disagrees ({label})"
            );
            (present, deadline)
        };

        // absent
        assert_eq!(check(1, "absent"), (false, None));

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
        assert_eq!(check(1, "live"), (true, Some(future)));

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
        assert_eq!(check(2, "expired"), (true, Some(past)));
    }

    #[test]
    fn expires_at_does_not_touch_hit_or_miss_counters_or_evictions() {
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .max_size(64)
            .shards(4)
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(
            &c,
            1u32,
            Val {
                v: 10,
                expired: false,
            },
        )
        .expect("insert must succeed");
        let before = c.metrics();

        let _ = ConcurrentCacheExpiry::cache_expires_at(&c, &1u32); // present
        let _ = ConcurrentCacheExpiry::cache_expires_at(&c, &2u32); // absent
        let _ = ConcurrentCacheExpiry::expires_at(&c, &1u32); // through the alias

        let after = c.metrics();
        assert_eq!(after.hits, before.hits, "the read must not count a hit");
        assert_eq!(
            after.misses, before.misses,
            "the read must not count a miss"
        );
        assert_eq!(
            after.evictions, before.evictions,
            "the read must not evict an entry"
        );
    }

    // Behavioral non-promotion: reading the LRU-tail key through `cache_expires_at` must not
    // touch its recency. max_size(2) + shards(1) gives an exact eviction bound: if the read
    // promoted key 1, inserting a third key would evict key 2 instead.
    #[test]
    fn expires_at_does_not_promote_lru() {
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .max_size(2)
            .shards(1)
            .build()
            .unwrap();

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

        // Read key 1 (currently LRU) through cache_expires_at -- must NOT promote it.
        let (present, _) = ConcurrentCacheExpiry::cache_expires_at(&c, &1u32);
        assert!(present);

        // Inserting key 3 must evict key 1 (still LRU because the read did not promote it).
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
            "key 1 must be evicted as LRU (cache_expires_at must not have promoted it)"
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
    fn expires_at_multi_shard() {
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .max_size(256)
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
            assert_eq!(
                ConcurrentCacheExpiry::cache_expires_at(&c, &i),
                (true, None),
                "key {i} must route to the right shard"
            );
        }
        assert_eq!(
            ConcurrentCacheExpiry::cache_expires_at(&c, &999u32),
            (false, None)
        );
    }

    #[test]
    fn expires_at_reports_absent_after_evict_removes_the_entry() {
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .max_size(64)
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(
            &c,
            1u32,
            Val {
                v: 100,
                expired: true,
            },
        )
        .expect("insert must succeed");

        assert_eq!(
            ConcurrentCacheExpiry::cache_expires_at(&c, &1u32),
            (true, None)
        );
        assert_eq!(
            c.evict(),
            1,
            "evict must physically remove the expired entry"
        );
        assert_eq!(
            ConcurrentCacheExpiry::cache_expires_at(&c, &1u32),
            (false, None),
            "a physically removed entry must be reported absent"
        );
    }

    #[test]
    fn expires_at_reports_absent_after_cache_remove() {
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .max_size(64)
            .build()
            .unwrap();
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

        assert_eq!(
            ConcurrentCacheExpiry::cache_expires_at(&c, &1u32),
            (false, None)
        );
        assert_eq!(ConcurrentCacheExpiry::expires_at(&c, &1u32), (false, None));
    }

    // The value-free read must be reachable through a generic `T: ConcurrentCacheExpiry<K, V>`
    // bound and through the prelude, exactly like its value-bearing sibling.
    #[test]
    fn concurrent_cache_expires_at_is_reachable_through_a_generic_bound_and_the_prelude() {
        use crate::prelude::*;

        fn deadline_via_bound<T: ConcurrentCacheExpiry<u32, Val>>(
            store: &T,
            key: &u32,
        ) -> (bool, Option<crate::time::Instant>) {
            store.cache_expires_at(key)
        }

        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .max_size(64)
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(
            &c,
            1,
            Val {
                v: 100,
                expired: false,
            },
        )
        .expect("insert must succeed");

        let (present, expires_at) = deadline_via_bound(&c, &1);
        assert!(present);
        assert_eq!(expires_at, None);
        assert_eq!(
            deadline_via_bound(&c, &2),
            (false, None),
            "absent key via the generic bound"
        );
    }

    // The point of moving `V: Clone` off the impl block and onto the value-bearing methods: a
    // deadline read must work on a cache whose value type is not `Clone` at all. The generic
    // helper carries no `V: Clone` bound anywhere, so this fails to compile if the bound creeps
    // back onto either the trait method or the impl.
    #[test]
    fn expires_at_reads_a_deadline_for_a_value_type_that_is_not_clone() {
        #[derive(Debug, PartialEq)]
        struct NotClone {
            v: u32,
            deadline: Option<crate::time::Instant>,
        }
        impl crate::Expires for NotClone {
            fn is_expired(&self) -> bool {
                false
            }
            fn expires_at(&self) -> Option<crate::time::Instant> {
                self.deadline
            }
        }

        fn deadline<K: Hash + Eq + Clone, V: Expires>(
            c: &ShardedExpiringLruCache<K, V>,
            k: &K,
        ) -> (bool, Option<crate::time::Instant>) {
            ConcurrentCacheExpiry::cache_expires_at(c, k)
        }

        let c = ShardedExpiringLruCache::<u32, NotClone>::builder()
            .max_size(64)
            .shards(4)
            .build()
            .unwrap();

        // Every insert path this store exposes requires `V: Clone`, so seed the shard's inner
        // `LruCache` directly via `Cached::cache_set` (no `V: Clone` bound there either). The
        // read under test is what this exercises.
        let stored = crate::time::Instant::now() + std::time::Duration::from_secs(60);
        c.shard_of(&1u32).lock.write().cache_set(
            1u32,
            NotClone {
                v: 100,
                deadline: Some(stored),
            },
        );
        c.shard_of(&2u32).lock.write().cache_set(
            2u32,
            NotClone {
                v: 200,
                deadline: None,
            },
        );

        assert_eq!(deadline(&c, &1), (true, Some(stored)), "live entry");
        assert_eq!(
            deadline(&c, &2),
            (true, None),
            "entry without an advisory deadline"
        );
        assert_eq!(deadline(&c, &3), (false, None), "absent key");
        // The alias is equally bound-free.
        assert!(ConcurrentCacheExpiry::expires_at(&c, &1).0);
        // The value was never cloned or moved out: it is still in the shard.
        use crate::CachedPeek;
        assert_eq!(
            c.shard_of(&1u32).lock.read().cache_peek(&1u32).map(|v| v.v),
            Some(100)
        );
    }

    // --- Inherent infallible method tests ---

    #[test]
    fn inherent_get_returns_option_not_result() {
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .max_size(64)
            .build()
            .unwrap();
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
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .max_size(64)
            .build()
            .unwrap();
        c.set(
            1,
            Val {
                v: 99,
                expired: true,
            },
        );
        let v: Option<Val> = c.get(&1);
        assert!(
            v.is_none(),
            "expired entry must return None from inherent get"
        );
    }

    #[test]
    fn inherent_set_returns_previous_value() {
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .max_size(64)
            .build()
            .unwrap();
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
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .max_size(64)
            .build()
            .unwrap();
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
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .shards(1)
            .max_size(64)
            .build()
            .unwrap();
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
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .max_size(64)
            .build()
            .unwrap();
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
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .max_size(64)
            .build()
            .unwrap();
        use_trait(
            &c,
            1,
            Val {
                v: 42,
                expired: false,
            },
        );
    }

    // B4 regression: deep_clone must load hit/miss counters under the read lock so the
    // metrics snapshot is consistent with the captured entry state.
    #[test]
    fn deep_clone_metrics_consistent_with_entry_snapshot() {
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .shards(1) // single shard: deterministic counters
            .max_size(16)
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
        // Generate exactly 3 hits and 2 misses.
        SyncConcurrentCached::cache_get(&c, &1).unwrap(); // hit
        SyncConcurrentCached::cache_get(&c, &1).unwrap(); // hit
        SyncConcurrentCached::cache_get(&c, &1).unwrap(); // hit
        SyncConcurrentCached::cache_get(&c, &99).unwrap(); // miss
        SyncConcurrentCached::cache_get(&c, &98).unwrap(); // miss

        let clone = c.deep_clone();
        let m = clone.metrics();
        assert_eq!(m.hits, Some(3), "deep_clone must capture the hit counter");
        assert_eq!(
            m.misses,
            Some(2),
            "deep_clone must capture the miss counter"
        );
        assert_eq!(clone.len(), 1, "deep_clone must capture the entry snapshot");
    }

    #[test]
    fn retain_preserves_survivor_recency_order() {
        // shards(1) so the recency order is deterministic and observable from one shard.
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .shards(1)
            .max_size(64)
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
    /// `LruCache`'s own capacity-eviction counter (`guard.evictions`).
    #[test]
    fn retain_wires_to_outer_evictions_not_inner_lru_counter() {
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .shards(1)
            .max_size(64)
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
            "retain must count each removal via the per-shard non-capacity counter"
        );
        assert_eq!(
            c.metrics().evictions.unwrap() - (inner_before + outer_before),
            5
        );
    }

    // --- single-lookup `cache_get`, per-shard eviction counters, and write recency ---

    fn live(v: u32) -> Val {
        Val { v, expired: false }
    }

    /// Raw per-shard non-capacity eviction counters, in shard order.
    fn shard_eviction_counters<K, V, H>(c: &ShardedExpiringLruCache<K, V, H>) -> Vec<u64> {
        c.inner
            .shards
            .iter()
            .map(|s| s.evictions.load(Ordering::Relaxed))
            .collect()
    }

    /// Index of the shard that owns `k`.
    fn owning_shard<K, V, H: ShardHasher<K>>(c: &ShardedExpiringLruCache<K, V, H>, k: &K) -> usize {
        shard_index(c.inner.hasher.shard_hash(k), c.inner.shard_mask)
    }

    /// Keys of one shard in MRU -> LRU order.
    fn shard_key_order<K: Clone + Hash + Eq, V: Clone, H>(
        c: &ShardedExpiringLruCache<K, V, H>,
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
    /// `pop_raw` only when that probe fails. All three outcomes must keep their old semantics:
    /// live hit -> value + hit; absent -> None + miss, nothing removed; expired -> None + miss,
    /// entry removed, one eviction counted, `on_evict` fired with the stored key/value.
    #[test]
    fn cache_get_single_lookup_keeps_hit_miss_expired_and_absent_semantics() {
        use std::sync::Mutex;
        let seen: Arc<Mutex<Vec<(u32, u32)>>> = Arc::new(Mutex::new(Vec::new()));
        let seen2 = Arc::clone(&seen);
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .shards(1)
            .max_size(8)
            .on_evict(move |k: &u32, v: &Val| seen2.lock().unwrap().push((*k, v.v)))
            .build()
            .unwrap();

        // Live hit.
        SyncConcurrentCached::cache_set(&c, 1, live(10)).unwrap();
        assert_eq!(
            SyncConcurrentCached::cache_get(&c, &1)
                .unwrap()
                .map(|v| v.v),
            Some(10)
        );
        assert_eq!(c.metrics().hits, Some(1));
        assert_eq!(c.metrics().misses, Some(0));
        assert_eq!(c.len(), 1, "a live hit must not remove anything");

        // Absent key: a miss, and the fall-through `pop_raw` must remove nothing.
        assert!(SyncConcurrentCached::cache_get(&c, &404).unwrap().is_none());
        assert_eq!(c.metrics().hits, Some(1));
        assert_eq!(c.metrics().misses, Some(1));
        assert_eq!(c.len(), 1);
        assert!(seen.lock().unwrap().is_empty(), "no eviction yet");
        let evictions_before = c.metrics().evictions.expect("evictions tracked");

        // Expired value: a miss, removed from the store, counted, callback fired.
        SyncConcurrentCached::cache_set(
            &c,
            2,
            Val {
                v: 20,
                expired: true,
            },
        )
        .unwrap();
        assert!(SyncConcurrentCached::cache_get(&c, &2).unwrap().is_none());
        assert_eq!(c.metrics().misses, Some(2));
        assert_eq!(c.len(), 1, "the expired entry must be removed on access");
        assert_eq!(*seen.lock().unwrap(), vec![(2, 20)]);
        assert_eq!(
            c.metrics().evictions.expect("evictions tracked") - evictions_before,
            1,
            "lazy expiry must count exactly one eviction"
        );
        // A second read of the now-absent key is a plain miss with no extra eviction.
        assert!(SyncConcurrentCached::cache_get(&c, &2).unwrap().is_none());
        assert_eq!(c.metrics().misses, Some(3));
        assert_eq!(
            c.metrics().evictions.expect("evictions tracked") - evictions_before,
            1
        );
        // The live entry is untouched throughout.
        assert_eq!(
            SyncConcurrentCached::cache_get(&c, &1)
                .unwrap()
                .map(|v| v.v),
            Some(10)
        );
    }

    /// The single-lookup path must still promote what it reads: `get_if` moves the entry to
    /// MRU when (and only when) the predicate reports the value live. Checked directly on the
    /// recency chain and through the capacity eviction it decides.
    #[test]
    fn cache_get_promotes_recency_through_the_single_lookup_path() {
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .shards(1)
            .max_size(2)
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1, live(10)).unwrap();
        SyncConcurrentCached::cache_set(&c, 2, live(20)).unwrap();
        assert_eq!(shard_key_order(&c, 0), vec![2, 1]);

        assert_eq!(
            SyncConcurrentCached::cache_get(&c, &1)
                .unwrap()
                .map(|v| v.v),
            Some(10)
        );
        assert_eq!(
            shard_key_order(&c, 0),
            vec![1, 2],
            "a live read must promote the entry to MRU"
        );

        // ... so the next capacity eviction claims key 2, not the just-read key 1.
        SyncConcurrentCached::cache_set(&c, 3, live(30)).unwrap();
        assert!(SyncConcurrentCached::cache_contains(&c, &1).unwrap());
        assert!(!SyncConcurrentCached::cache_contains(&c, &2).unwrap());
    }

    /// `cache_set` with an `on_evict` callback recovers the stored key through
    /// `cache_set_returning_entry`, which promotes the overwritten entry to MRU. This test
    /// fails if that promotion is dropped.
    #[test]
    fn cache_set_with_on_evict_promotes_overwritten_entry_to_mru() {
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .shards(1)
            .max_size(2)
            .on_evict(|_, _| {})
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1, live(10)).unwrap();
        SyncConcurrentCached::cache_set(&c, 2, live(20)).unwrap();
        assert_eq!(shard_key_order(&c, 0), vec![2, 1]);

        // Overwrite the LRU entry: it must come back as MRU, and return the old value.
        assert_eq!(
            SyncConcurrentCached::cache_set(&c, 1, live(11))
                .unwrap()
                .map(|v| v.v),
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
        SyncConcurrentCached::cache_set(&c, 3, live(30)).unwrap();
        assert_eq!(
            SyncConcurrentCached::cache_get(&c, &1)
                .unwrap()
                .map(|v| v.v),
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
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .shards(1)
            .max_size(2)
            .build()
            .unwrap();
        SyncConcurrentCached::cache_set(&c, 1, live(10)).unwrap();
        SyncConcurrentCached::cache_set(&c, 2, live(20)).unwrap();
        assert_eq!(
            SyncConcurrentCached::cache_set(&c, 1, live(11))
                .unwrap()
                .map(|v| v.v),
            Some(10)
        );
        assert_eq!(
            shard_key_order(&c, 0),
            vec![1, 2],
            "an overwrite promotes to MRU with or without an on_evict callback"
        );

        // ... and the promotion decides the next capacity eviction victim, exactly as it
        // does on the `on_evict` path.
        SyncConcurrentCached::cache_set(&c, 3, live(30)).unwrap();
        assert!(SyncConcurrentCached::cache_contains(&c, &1).unwrap());
        assert!(!SyncConcurrentCached::cache_contains(&c, &2).unwrap());
    }

    /// Overwriting the current MRU entry (already the head of the LRU chain) must leave it
    /// at the head with the chain intact, on both `cache_set` branches.
    #[test]
    fn cache_set_over_current_mru_keeps_it_at_the_front() {
        for with_on_evict in [false, true] {
            let builder = ShardedExpiringLruCache::<u32, Val>::builder()
                .shards(1)
                .max_size(3);
            let c = if with_on_evict {
                builder.on_evict(|_, _| {}).build().unwrap()
            } else {
                builder.build().unwrap()
            };
            for k in 1..=3u32 {
                SyncConcurrentCached::cache_set(&c, k, live(k * 10)).unwrap();
            }
            assert_eq!(shard_key_order(&c, 0), vec![3, 2, 1]);
            assert_eq!(
                SyncConcurrentCached::cache_set(&c, 3, live(33))
                    .unwrap()
                    .map(|v| v.v),
                Some(30)
            );
            assert_eq!(
                shard_key_order(&c, 0),
                vec![3, 2, 1],
                "on_evict={with_on_evict}: overwriting the head must keep it at the head"
            );
            assert_eq!(c.len(), 3);
            // The chain is intact: the LRU victim is still key 1.
            SyncConcurrentCached::cache_set(&c, 4, live(40)).unwrap();
            assert_eq!(shard_key_order(&c, 0), vec![4, 3, 2]);
            assert!(!SyncConcurrentCached::cache_contains(&c, &1).unwrap());
        }
    }

    /// A 1-capacity shard: overwriting the sole entry must not corrupt the chain.
    #[test]
    fn cache_set_over_sole_entry_of_capacity_one_shard() {
        for with_on_evict in [false, true] {
            let builder = ShardedExpiringLruCache::<u32, Val>::builder()
                .shards(1)
                .max_size(1);
            let c = if with_on_evict {
                builder.on_evict(|_, _| {}).build().unwrap()
            } else {
                builder.build().unwrap()
            };
            SyncConcurrentCached::cache_set(&c, 1, live(10)).unwrap();
            assert_eq!(
                SyncConcurrentCached::cache_set(&c, 1, live(11))
                    .unwrap()
                    .map(|v| v.v),
                Some(10),
                "on_evict={with_on_evict}"
            );
            assert_eq!(shard_key_order(&c, 0), vec![1]);
            assert_eq!(c.len(), 1);
            SyncConcurrentCached::cache_set(&c, 2, live(20)).unwrap();
            assert_eq!(shard_key_order(&c, 0), vec![2]);
            assert_eq!(c.len(), 1);
        }
    }

    /// Every eviction counted outside the inner LRU counter lands on the shard that owns the
    /// key, and `metrics()` aggregates the per-shard counters together with the inner LRU
    /// counters without double-counting.
    #[test]
    fn every_eviction_path_counts_on_the_owning_shard() {
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .shards(8)
            .per_shard_max_size(8)
            .build()
            .unwrap();
        let mut expected = vec![0u64; 8];
        let expired = |v: u32| Val { v, expired: true };

        // 1) Lazy expiry through cache_get.
        SyncConcurrentCached::cache_set(&c, 1, expired(10)).unwrap();
        assert!(SyncConcurrentCached::cache_get(&c, &1).unwrap().is_none());
        expected[owning_shard(&c, &1)] += 1;
        assert_eq!(
            shard_eviction_counters(&c),
            expected,
            "cache_get lazy expiry"
        );

        // 2) evict() sweep.
        SyncConcurrentCached::cache_set(&c, 2, expired(20)).unwrap();
        SyncConcurrentCached::cache_set(&c, 3, expired(30)).unwrap();
        assert_eq!(ConcurrentCacheEvict::evict(&c), 2);
        expected[owning_shard(&c, &2)] += 1;
        expected[owning_shard(&c, &3)] += 1;
        assert_eq!(shard_eviction_counters(&c), expected, "evict");

        // 3) retain().
        SyncConcurrentCached::cache_set(&c, 4, live(40)).unwrap();
        c.retain(|_k, _v| false);
        expected[owning_shard(&c, &4)] += 1;
        assert_eq!(shard_eviction_counters(&c), expected, "retain");

        // 4) Explicit removes stay on the INNER per-shard LruCache counter (unchanged split).
        SyncConcurrentCached::cache_set(&c, 5, live(50)).unwrap();
        let non_capacity_before = shard_eviction_counters(&c);
        let total_before = c.metrics().evictions.expect("evictions tracked");
        assert!(
            SyncConcurrentCached::cache_remove(&c, &5)
                .unwrap()
                .is_some()
        );
        assert_eq!(
            shard_eviction_counters(&c),
            non_capacity_before,
            "cache_remove must keep counting on the inner LRU counter"
        );
        assert_eq!(
            c.metrics().evictions,
            Some(total_before + 1),
            "the removal must still show up in the aggregate exactly once"
        );

        // 5) cache_clear_with_on_evict also counts on the inner counter for this store.
        SyncConcurrentCached::cache_set(&c, 6, live(60)).unwrap();
        let non_capacity_before = shard_eviction_counters(&c);
        let total_before = c.metrics().evictions.expect("evictions tracked");
        c.cache_clear_with_on_evict();
        assert_eq!(shard_eviction_counters(&c), non_capacity_before);
        assert_eq!(c.metrics().evictions, Some(total_before + 1));

        // 6) Capacity evictions: inner counter only, added on top by metrics().
        let victims: Vec<u32> = (0..1000u32)
            .filter(|i| owning_shard(&c, i) == 0)
            .take(20)
            .collect();
        assert_eq!(victims.len(), 20, "need 20 keys landing on shard 0");
        let non_capacity_before = shard_eviction_counters(&c);
        let total_before = c.metrics().evictions.expect("evictions tracked");
        for k in &victims {
            SyncConcurrentCached::cache_set(&c, *k, live(*k)).unwrap();
        }
        assert_eq!(
            shard_eviction_counters(&c),
            non_capacity_before,
            "capacity evictions must NOT touch the non-capacity counters"
        );
        assert_eq!(
            c.metrics().evictions,
            Some(total_before + 12),
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
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .shards(4)
            .per_shard_max_size(4)
            .build()
            .unwrap();
        // Non-capacity evictions (lazy expiry) plus capacity evictions from overfilling.
        for i in 0..4u32 {
            SyncConcurrentCached::cache_set(
                &c,
                i,
                Val {
                    v: i,
                    expired: true,
                },
            )
            .unwrap();
            assert!(SyncConcurrentCached::cache_get(&c, &i).unwrap().is_none());
        }
        for i in 100..140u32 {
            SyncConcurrentCached::cache_set(&c, i, live(i)).unwrap();
        }
        let before = c.metrics().evictions.expect("evictions tracked");
        let per_shard_before = shard_eviction_counters(&c);
        assert_eq!(per_shard_before.iter().sum::<u64>(), 4);
        assert!(
            before > 4,
            "the fixture must also produce capacity evictions"
        );

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

        // The clone is independent: further non-capacity evictions do not touch the source.
        SyncConcurrentCached::cache_set(
            &cloned,
            999,
            Val {
                v: 999,
                expired: true,
            },
        )
        .unwrap();
        assert!(
            SyncConcurrentCached::cache_get(&cloned, &999)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            shard_eviction_counters(&cloned).iter().sum::<u64>(),
            5,
            "the lazy expiry must count on the clone's own shard counter"
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
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .shards(1)
            .max_size(64)
            .on_evict(move |k: &u32, _v: &Val| seen2.lock().unwrap().push(*k))
            .build()
            .unwrap();
        for i in 0..6u32 {
            SyncConcurrentCached::cache_set(&c, i, live(i)).unwrap();
        }
        assert!(SyncConcurrentCached::cache_get(&c, &0).unwrap().is_some());
        assert!(SyncConcurrentCached::cache_get(&c, &2).unwrap().is_some());
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
        SyncConcurrentCached::cache_set(&c, 42, live(42)).unwrap();
        assert!(SyncConcurrentCached::cache_get(&c, &42).unwrap().is_some());
    }
}

/// Borrowed-key (`Borrow<Q>`) inherent lookups and the concurrent capability traits.
#[cfg(test)]
mod borrowed_key_and_capability_tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering as AOrd};

    /// A never-expiring value: this module exercises key routing, not expiry.
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Live(u32);
    impl crate::Expires for Live {
        fn is_expired(&self) -> bool {
            false
        }
    }

    fn keys() -> Vec<String> {
        (0..200).map(|i| format!("key-{i}")).collect()
    }

    fn val(i: usize) -> Live {
        Live(i as u32)
    }

    /// A multi-shard, `String`-keyed store with every key inserted by value.
    fn filled() -> (ShardedExpiringLruCache<String, Live>, Vec<String>) {
        let c = ShardedExpiringLruCache::<String, Live>::builder()
            .shards(8)
            .max_size(1024)
            .build()
            .unwrap();
        let ks = keys();
        for (i, k) in ks.iter().enumerate() {
            c.set(k.clone(), val(i));
        }
        assert!(
            c.shard_sizes().iter().filter(|n| **n > 0).count() > 1,
            "the borrowed-key tests are only meaningful across several shards: {:?}",
            c.shard_sizes()
        );
        (c, ks)
    }

    /// Position of `shard` within the store's shard array, found by address.
    ///
    /// Used only so the parity assertions below can report that they spanned several shards. The
    /// assertions themselves compare what the store's own `shard_of` and `shard_of_borrowed`
    /// return, by address. Recomputing the routing formula in the test instead would assert a
    /// property of `DefaultShardHasher` rather than of this store, and would keep passing if
    /// `shard_of_borrowed`'s body were replaced with `&self.inner.shards[0]`.
    fn shard_position<S>(shards: &[CachePadded<Shard<S>>], shard: &CachePadded<Shard<S>>) -> usize {
        shards
            .iter()
            .position(|s| std::ptr::eq(s, shard))
            .expect("a shard returned by the store's own router must be one of its shards")
    }

    /// An owned key and the equivalent borrowed key must select the same shard. A silent
    /// mismatch here is exactly the failure mode this feature risks: the entry is present, the
    /// lookup lands on the wrong shard, and the store reports a miss.
    #[test]
    fn owned_and_borrowed_keys_route_to_the_same_shard() {
        let (c, ks) = filled();
        let mut seen = HashSet::new();
        for k in &ks {
            let owned = c.shard_of(k);
            assert!(
                std::ptr::eq(owned, c.shard_of_borrowed(k.as_str())),
                "`{k}` routes to a different shard as `&str` than as `String`"
            );
            seen.insert(shard_position(&c.inner.shards, owned));
        }
        assert!(
            seen.len() > 1,
            "routing parity must be checked across shards, saw {seen:?}"
        );
    }

    /// Routing parity for a newtype over a primitive: `UserId(u64)` with `Borrow<u64>`.
    ///
    /// Compares what this store's own `shard_of` and `shard_of_borrowed` return, by address, the
    /// same way `owned_and_borrowed_keys_route_to_the_same_shard` above does. This store builds
    /// with the default hasher, and routing goes through `routing_hash` (`build_hasher` +
    /// `Hash::hash` + `Hasher::finish`), which never calls `BuildHasher::hash_one`, so the
    /// `hash_one` specialization hazard -- `ahash::RandomState` dispatching a different
    /// `CallHasher` impl for `&UserId` than for `&u64` under nightly's `specialize` cfg -- is not
    /// reachable from here. That hazard is covered by a dedicated hasher-override case in
    /// `tests/sharded_newtype_key_routing_parity.rs`.
    ///
    /// The `String`/`&str` and `Vec<u8>`/`&[u8]` cases above only ever exercise ahash's generic
    /// `CallHasher` path: ahash has no specialized impl for either shape, so those two cases would
    /// pass identically even if `shard_of_borrowed` regressed to `BuildHasher::hash_one`. A
    /// newtype over `u64` is the only key shape here that discriminates.
    #[test]
    fn newtype_over_primitive_routes_the_same_owned_and_borrowed() {
        #[derive(Clone, Debug, PartialEq, Eq)]
        struct UserId(u64);
        impl std::hash::Hash for UserId {
            fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
                self.0.hash(state);
            }
        }
        impl std::borrow::Borrow<u64> for UserId {
            fn borrow(&self) -> &u64 {
                &self.0
            }
        }

        let c = ShardedExpiringLruCache::<UserId, Live>::builder()
            .shards(8)
            .max_size(1024)
            .build()
            .unwrap();
        for id in 0..200u64 {
            c.set(UserId(id), val(id as usize));
        }

        let mut seen = HashSet::new();
        for id in 0..200u64 {
            let owned = c.shard_of(&UserId(id));
            assert!(
                std::ptr::eq(owned, c.shard_of_borrowed(&id)),
                "`UserId({id})` routes to a different shard than its borrowed `u64`"
            );
            seen.insert(shard_position(&c.inner.shards, owned));
            assert_eq!(
                c.get(&id),
                Some(val(id as usize)),
                "borrowed `get` with a `&u64` missed `UserId({id})`"
            );
        }
        assert!(
            seen.len() > 1,
            "newtype routing parity must be checked across shards, saw {seen:?}"
        );
    }

    /// Routing parity for a plain `BuildHasher` that is not `DefaultShardHasher`.
    ///
    /// The cases above only ever exercise `DefaultShardHasher`, which is in-tree. This one pins
    /// the same agreement for `std::hash::RandomState`, which reaches `ShardHasher` through the
    /// blanket impl and nothing else, and follows it with a borrowed `get` / `remove` round trip
    /// so the parity is observed through the public surface as well as by address.
    #[test]
    fn owned_and_borrowed_keys_route_together_for_a_plain_build_hasher() {
        let c = ShardedExpiringLruCache::<String, Live>::builder()
            .shards(8)
            .max_size(1024)
            .hasher(std::hash::RandomState::new())
            .build()
            .unwrap();
        let ks = keys();
        for (i, k) in ks.iter().enumerate() {
            c.set(k.clone(), val(i));
        }
        assert!(
            c.shard_sizes().iter().filter(|n| **n > 0).count() > 1,
            "the parity check is only meaningful across several shards: {:?}",
            c.shard_sizes()
        );

        let mut seen = HashSet::new();
        for (i, k) in ks.iter().enumerate() {
            let owned = c.shard_of(k);
            assert!(
                std::ptr::eq(owned, c.shard_of_borrowed(k.as_str())),
                "`{k}` routes to a different shard as `&str` than as `String` under `RandomState`"
            );
            seen.insert(shard_position(&c.inner.shards, owned));
            assert_eq!(
                c.get(k.as_str()),
                Some(val(i)),
                "borrowed `get` missed `{k}` under a plain `BuildHasher`"
            );
        }
        assert!(
            seen.len() > 1,
            "routing parity must be checked across shards, saw {seen:?}"
        );

        for (i, k) in ks.iter().enumerate() {
            assert_eq!(
                c.remove(k.as_str()),
                Some(val(i)),
                "borrowed `remove` missed `{k}` under a plain `BuildHasher`"
            );
        }
        assert!(
            c.is_empty(),
            "every entry must have been removed through the borrowed key"
        );
    }

    /// `Vec<u8>` / `&[u8]` key parity, alongside the `String` / `&str` shape above. A byte-slice
    /// key forwards `Hash` differently from a `str` key, so the owned/borrowed routing agreement
    /// is pinned in its own right rather than inferred from the `String` case.
    #[test]
    fn owned_and_borrowed_byte_slice_keys_route_to_the_same_shard() {
        let c = ShardedExpiringLruCache::<Vec<u8>, Live>::builder()
            .shards(8)
            .max_size(1024)
            .build()
            .unwrap();
        let ks: Vec<Vec<u8>> = (0..200).map(|i| format!("key-{i}").into_bytes()).collect();
        for (i, k) in ks.iter().enumerate() {
            c.set(k.clone(), val(i));
        }
        assert!(
            c.shard_sizes().iter().filter(|n| **n > 0).count() > 1,
            "byte-key routing parity is only meaningful across several shards: {:?}",
            c.shard_sizes()
        );

        let mut seen = HashSet::new();
        for (i, k) in ks.iter().enumerate() {
            let owned = c.shard_of(k);
            assert!(
                std::ptr::eq(owned, c.shard_of_borrowed(k.as_slice())),
                "`{k:?}` routes to a different shard as `&[u8]` than as `Vec<u8>`"
            );
            seen.insert(shard_position(&c.inner.shards, owned));
            assert_eq!(
                c.get(k.as_slice()),
                Some(val(i)),
                "borrowed `get` with a `&[u8]` missed `{k:?}`"
            );
        }
        assert!(
            seen.len() > 1,
            "byte-key routing parity must be checked across shards, saw {seen:?}"
        );
    }

    #[test]
    fn borrowed_get_finds_every_owned_entry_across_shards() {
        let (c, ks) = filled();
        let before = c.metrics();
        for (i, k) in ks.iter().enumerate() {
            assert_eq!(
                c.get(k.as_str()),
                Some(val(i)),
                "borrowed `get` missed `{k}`"
            );
        }
        let after = c.metrics();
        assert_eq!(
            after.hits.unwrap() - before.hits.unwrap(),
            ks.len() as u64,
            "every borrowed hit must be counted as a hit"
        );
        assert_eq!(
            after.misses.unwrap(),
            before.misses.unwrap(),
            "no borrowed lookup may be recorded as a miss"
        );
    }

    #[test]
    fn borrowed_get_misses_an_absent_key_and_counts_a_miss() {
        let (c, _) = filled();
        let before = c.metrics();
        assert_eq!(c.get("absent"), None);
        let after = c.metrics();
        assert_eq!(after.misses.unwrap() - before.misses.unwrap(), 1);
        assert_eq!(after.hits.unwrap(), before.hits.unwrap());
    }

    #[test]
    fn borrowed_contains_and_peek_agree_with_borrowed_get() {
        let (c, ks) = filled();
        for (i, k) in ks.iter().enumerate() {
            assert!(c.contains(k.as_str()), "borrowed `contains` missed `{k}`");
            assert_eq!(
                c.peek(k.as_str()),
                Some(val(i)),
                "borrowed `peek` missed `{k}`"
            );
            assert_eq!(c.peek(k.as_str()), c.get(k.as_str()));
        }
        assert!(!c.contains("absent"));
        assert_eq!(c.peek("absent"), None);
    }

    #[test]
    fn borrowed_contains_and_peek_record_no_hit_or_miss() {
        let (c, ks) = filled();
        let before = c.metrics();
        assert!(c.contains(ks[0].as_str()));
        assert!(c.peek(ks[0].as_str()).is_some());
        assert!(!c.contains("absent"));
        assert_eq!(c.peek("absent"), None);
        let after = c.metrics();
        assert_eq!(after.hits, before.hits);
        assert_eq!(after.misses, before.misses);
    }

    #[test]
    fn borrowed_remove_returns_the_value_and_removes_the_entry() {
        let (c, ks) = filled();
        let before = c.len();
        for (i, k) in ks.iter().enumerate() {
            assert_eq!(
                c.remove(k.as_str()),
                Some(val(i)),
                "borrowed `remove` missed `{k}`"
            );
        }
        assert_eq!(c.len(), before - ks.len());
        assert_eq!(
            c.remove(ks[0].as_str()),
            None,
            "a second remove must find nothing"
        );
    }

    #[test]
    fn borrowed_remove_entry_returns_the_stored_owned_key() {
        let (c, ks) = filled();
        for (i, k) in ks.iter().enumerate() {
            assert_eq!(
                c.remove_entry(k.as_str()),
                Some((k.clone(), val(i))),
                "borrowed `remove_entry` missed `{k}`"
            );
        }
        assert!(c.is_empty());
    }

    #[test]
    fn borrowed_delete_reports_and_removes() {
        let (c, ks) = filled();
        for k in &ks {
            assert!(c.delete(k.as_str()), "borrowed `delete` missed `{k}`");
            assert!(!c.contains(k.as_str()));
        }
        assert!(c.is_empty());
        assert!(!c.delete(ks[0].as_str()));
    }

    #[test]
    fn borrowed_remove_fires_on_evict_with_the_stored_key() {
        let seen: Arc<parking_lot::Mutex<Vec<String>>> =
            Arc::new(parking_lot::Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        let c = ShardedExpiringLruCache::<String, Live>::builder()
            .shards(8)
            .max_size(1024)
            .on_evict(move |k: &String, _v: &Live| seen2.lock().push(k.clone()))
            .build()
            .unwrap();
        c.set("a".to_string(), val(1));
        assert!(c.remove("a").is_some());
        assert_eq!(&*seen.lock(), &["a".to_string()]);
    }

    /// A value that reports itself expired, for the expired-remove path. The rest of this module
    /// uses `Live` because it exercises key routing, not expiry.
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Perishable {
        v: u32,
        expired: bool,
    }
    impl crate::Expires for Perishable {
        fn is_expired(&self) -> bool {
            self.expired
        }
    }

    #[allow(clippy::type_complexity)]
    fn store_with_one_expired_value() -> (
        ShardedExpiringLruCache<String, Perishable>,
        Arc<parking_lot::Mutex<Vec<(String, u32)>>>,
    ) {
        let seen: Arc<parking_lot::Mutex<Vec<(String, u32)>>> =
            Arc::new(parking_lot::Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        let c = ShardedExpiringLruCache::<String, Perishable>::builder()
            .shards(8)
            .max_size(1024)
            .on_evict(move |k: &String, v: &Perishable| seen2.lock().push((k.clone(), v.v)))
            .build()
            .unwrap();
        c.set(
            "stale".to_string(),
            Perishable {
                v: 7,
                expired: true,
            },
        );
        (c, seen)
    }

    /// A value that counts how many times `is_expired()` is called on it.
    ///
    /// `Expires::is_expired` is arbitrary user code with no purity guarantee, so which operations
    /// invoke it is part of the observable contract, not an implementation detail.
    #[derive(Clone, Debug)]
    struct Counted {
        expired: bool,
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }
    impl crate::Expires for Counted {
        fn is_expired(&self) -> bool {
            self.calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.expired
        }
    }

    /// Build a store holding one live `Counted` entry, plus the shared call counter.
    ///
    /// `max_size` is far above the entry count so no capacity eviction runs, and no `on_evict` is
    /// configured, so nothing but the operation under test can touch the counter.
    fn store_with_one_counted_value() -> (
        ShardedExpiringLruCache<String, Counted>,
        Arc<std::sync::atomic::AtomicUsize>,
    ) {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c = ShardedExpiringLruCache::<String, Counted>::builder()
            .shards(8)
            .max_size(1024)
            .build()
            .unwrap();
        c.set(
            "k".to_string(),
            Counted {
                expired: false,
                calls: calls.clone(),
            },
        );
        (c, calls)
    }

    /// `remove_entry` and `delete` hand the caller an entry that is leaving the cache either way,
    /// so neither consults `is_expired()`. Both route through `pop_in`, which does not call it;
    /// only `remove_in` does, because `remove` alone filters an expired value back out to `None`.
    /// Nothing else pins this, so a regression that reintroduces the call in `pop_in` would be
    /// silent.
    #[test]
    fn remove_entry_and_delete_do_not_consult_is_expired() {
        let load =
            |c: &Arc<std::sync::atomic::AtomicUsize>| c.load(std::sync::atomic::Ordering::Relaxed);

        let (c, calls) = store_with_one_counted_value();
        calls.store(0, std::sync::atomic::Ordering::Relaxed);
        assert!(
            c.remove_entry("k").is_some(),
            "the entry must actually be present, or the count below is vacuous"
        );
        assert_eq!(
            load(&calls),
            0,
            "`remove_entry` must not call `is_expired()` on the value it hands back"
        );

        let (c, calls) = store_with_one_counted_value();
        calls.store(0, std::sync::atomic::Ordering::Relaxed);
        assert!(c.delete("k"), "the entry must actually be present");
        assert_eq!(
            load(&calls),
            0,
            "`delete` must not call `is_expired()` on the removed value"
        );

        // The contrast case: `remove` does filter on expiry, so it calls the user code exactly
        // once. A change making `pop_in` consult expiry again would push this above 1.
        let (c, calls) = store_with_one_counted_value();
        calls.store(0, std::sync::atomic::Ordering::Relaxed);
        assert!(
            c.remove("k").is_some(),
            "the entry must actually be present"
        );
        assert_eq!(
            load(&calls),
            1,
            "`remove` filters expired values, so it calls `is_expired()` exactly once"
        );
    }

    /// The expired-on-access branch of `get_in`, which holds the shard write lock throughout: the
    /// `get_if` probe finds no live value and `pop_raw` removes the still-stored expired entry
    /// under that same lock, without ever dropping to a read lock. An eviction is counted,
    /// `on_evict` fires and the call still reports a miss. Only owned-key tests reached that
    /// branch; this is the borrowed one. `contains` and `peek` take the read-only path over the
    /// same entry: they must report it absent without removing it or notifying anyone.
    #[test]
    fn borrowed_get_of_an_expired_value_evicts_and_misses() {
        let (c, seen) = store_with_one_expired_value();
        let before = c.metrics();

        assert!(
            !c.contains("stale"),
            "a borrowed `contains` must not report an expired value as present"
        );
        assert_eq!(
            c.peek("stale"),
            None,
            "a borrowed `peek` must not hand back an expired value"
        );
        assert_eq!(
            c.len(),
            1,
            "neither `contains` nor `peek` may remove the expired entry"
        );
        assert!(
            seen.lock().is_empty(),
            "neither `contains` nor `peek` may fire `on_evict`"
        );
        assert_eq!(
            c.metrics().evictions,
            before.evictions,
            "neither `contains` nor `peek` may count an eviction"
        );

        assert_eq!(
            c.get("stale"),
            None,
            "a borrowed `get` of an expired value must miss"
        );

        let after = c.metrics();
        assert!(
            c.is_empty(),
            "the expired entry must have been removed by the `get`"
        );
        assert_eq!(
            &*seen.lock(),
            &[("stale".to_string(), 7)],
            "exactly one `on_evict`, carrying the stored key and the expired value"
        );
        assert_eq!(
            after.evictions.unwrap() - before.evictions.unwrap(),
            1,
            "the expired-on-access removal must count exactly one eviction"
        );
        assert_eq!(
            after.misses.unwrap() - before.misses.unwrap(),
            1,
            "the expired-on-access `get` must count exactly one miss"
        );
        assert_eq!(after.hits, before.hits, "an expired `get` is never a hit");
    }

    /// `remove` / `remove_entry` / `delete` all funnel through one `pop_in` helper that fires
    /// `on_evict` and bumps the eviction counter without consulting `Expires::is_expired` at all;
    /// only `remove_in` checks expiry, on the value `pop_in` hands back. So an expired value is
    /// still handed to the callback and still counted, even though `remove` itself reports `None`
    /// (there was no live value to hand back). Only the owned entry points covered that ordering;
    /// this is the borrowed one.
    #[test]
    fn borrowed_remove_of_an_expired_value_reports_none_but_still_evicts() {
        let (c, seen) = store_with_one_expired_value();
        let before = c.metrics();

        assert_eq!(
            c.remove("stale"),
            None,
            "a borrowed remove of an expired value must report no live value"
        );

        let after = c.metrics();
        assert!(
            c.is_empty(),
            "the expired entry must still be removed from its shard"
        );
        assert_eq!(
            &*seen.lock(),
            &[("stale".to_string(), 7)],
            "on_evict fires before is_expired is consulted, so the expired value is still reported"
        );
        assert_eq!(
            after.evictions.unwrap() - before.evictions.unwrap(),
            1,
            "the removal of an expired entry must still be counted"
        );

        // A second borrowed remove finds nothing and must not double-count or re-notify.
        assert_eq!(c.remove("stale"), None);
        assert_eq!(c.metrics().evictions.unwrap(), after.evictions.unwrap());
        assert_eq!(seen.lock().len(), 1);
    }

    /// The other two `pop_in` callers keep their own contract on the same expired value:
    /// `remove_entry` hands back the stored pair regardless of expiry, and `delete` reports
    /// `true` because an entry was physically removed.
    #[test]
    fn borrowed_remove_entry_and_delete_of_an_expired_value_still_report_the_removal() {
        let (c, seen) = store_with_one_expired_value();
        assert_eq!(
            c.remove_entry("stale"),
            Some((
                "stale".to_string(),
                Perishable {
                    v: 7,
                    expired: true
                }
            )),
            "remove_entry returns the stored pair even when the value is expired"
        );
        assert_eq!(&*seen.lock(), &[("stale".to_string(), 7)]);
        assert_eq!(c.metrics().evictions, Some(1));

        let (c, seen) = store_with_one_expired_value();
        assert!(
            c.delete("stale"),
            "delete reports whether an entry was removed, not whether it was live"
        );
        assert!(c.is_empty());
        assert_eq!(&*seen.lock(), &[("stale".to_string(), 7)]);
        assert_eq!(c.metrics().evictions, Some(1));
        assert!(!c.delete("stale"));
    }

    #[test]
    fn borrowed_peek_leaves_lru_recency_alone_but_borrowed_get_promotes() {
        // One shard with an exact cap of 2, so the eviction victim pins recency exactly.
        let peeked = ShardedExpiringLruCache::<String, Live>::builder()
            .shards(1)
            .max_size(2)
            .build()
            .unwrap();
        peeked.set("a".to_string(), val(1));
        peeked.set("b".to_string(), val(2));
        assert_eq!(peeked.peek("a"), Some(val(1)));
        peeked.set("c".to_string(), val(3));
        assert_eq!(
            peeked.peek("a"),
            None,
            "a borrowed peek must not save the LRU entry, matching the owned peek"
        );
        assert_eq!(peeked.peek("b"), Some(val(2)));
        assert_eq!(peeked.peek("c"), Some(val(3)));

        let gotten = ShardedExpiringLruCache::<String, Live>::builder()
            .shards(1)
            .max_size(2)
            .build()
            .unwrap();
        gotten.set("a".to_string(), val(1));
        gotten.set("b".to_string(), val(2));
        assert_eq!(gotten.get("a"), Some(val(1)));
        gotten.set("c".to_string(), val(3));
        assert_eq!(
            gotten.peek("a"),
            Some(val(1)),
            "a borrowed get must promote recency, matching the owned get"
        );
        assert_eq!(gotten.peek("b"), None, "`b` is now the LRU victim");
    }

    // `set_max_size` / `try_set_max_size` / `cache_clear_with_on_evict` are also inherent
    // methods, and inherent methods win at a concrete call site. These helpers take a generic
    // bound, so they can only reach the trait method: they are the reachability the traits add.
    fn resize_through_trait<T: crate::ConcurrentCacheSetMaxSize>(
        cache: &T,
        max_size: usize,
    ) -> Option<usize> {
        cache.set_max_size(max_size)
    }

    fn try_resize_through_trait<T: crate::ConcurrentCacheSetMaxSize>(
        cache: &T,
        max_size: usize,
    ) -> Result<Option<usize>, crate::SetMaxSizeError> {
        cache.try_set_max_size(max_size)
    }

    fn clear_with_on_evict_through_trait<T: crate::ConcurrentCacheClearWithOnEvict>(cache: &T) {
        cache.cache_clear_with_on_evict();
    }

    #[test]
    fn cache_clear_with_on_evict_through_trait_fires_for_all_entries() {
        let fired = Arc::new(AtomicUsize::new(0));
        let fired2 = fired.clone();
        let c = ShardedExpiringLruCache::<String, Live>::builder()
            .shards(4)
            .max_size(1024)
            .on_evict(move |_k: &String, _v: &Live| {
                fired2.fetch_add(1, AOrd::Relaxed);
            })
            .build()
            .unwrap();
        for (i, k) in keys().iter().take(12).enumerate() {
            c.set(k.clone(), val(i));
        }
        assert_eq!(c.len(), 12);

        clear_with_on_evict_through_trait(&c);
        assert_eq!(c.len(), 0);
        assert_eq!(fired.load(AOrd::Relaxed), 12);
        assert_eq!(c.metrics().evictions, Some(12));
    }

    #[test]
    fn set_max_size_through_trait_shrinks_eagerly_and_fires_on_evict() {
        let fired = Arc::new(AtomicUsize::new(0));
        let fired2 = fired.clone();
        let c = ShardedExpiringLruCache::<String, Live>::builder()
            .shards(1)
            .max_size(4)
            .on_evict(move |_k: &String, _v: &Live| {
                fired2.fetch_add(1, AOrd::Relaxed);
            })
            .build()
            .unwrap();
        for (i, k) in ["a", "b", "c", "d"].iter().enumerate() {
            c.set((*k).to_string(), val(i));
        }

        assert_eq!(resize_through_trait(&c, 2), Some(4));
        assert_eq!(c.capacity(), 2);
        // Eviction happens before the call returns, not on the next insert.
        assert_eq!(c.len(), 2);
        assert_eq!(fired.load(AOrd::Relaxed), 2);
        assert_eq!(c.metrics().evictions, Some(2));
        assert_eq!(c.peek("c"), Some(val(2)));
        assert_eq!(c.peek("d"), Some(val(3)));
        assert_eq!(c.peek("a"), None);
        assert_eq!(c.peek("b"), None);
    }

    #[test]
    fn set_max_size_through_trait_grows_without_evicting() {
        let c = ShardedExpiringLruCache::<String, Live>::builder()
            .shards(1)
            .max_size(2)
            .build()
            .unwrap();
        c.set("a".to_string(), val(1));
        c.set("b".to_string(), val(2));
        assert_eq!(resize_through_trait(&c, 8), Some(2));
        assert_eq!(c.capacity(), 8);
        assert_eq!(c.len(), 2);
        assert_eq!(c.peek("a"), Some(val(1)));
    }

    #[test]
    fn try_set_max_size_through_trait_rejects_zero() {
        let c = ShardedExpiringLruCache::<String, Live>::builder()
            .shards(1)
            .max_size(4)
            .build()
            .unwrap();
        assert_eq!(
            try_resize_through_trait(&c, 0),
            Err(crate::SetMaxSizeError::ZeroMaxSize)
        );
        assert_eq!(
            c.capacity(),
            4,
            "a rejected resize must not change the bound"
        );
        assert_eq!(try_resize_through_trait(&c, 6), Ok(Some(4)));
        assert_eq!(c.capacity(), 6);
    }

    #[test]
    fn try_set_max_size_through_trait_rejects_capacity_overflow() {
        let c = ShardedExpiringLruCache::<String, Live>::builder()
            .shards(4)
            .max_size(64)
            .build()
            .unwrap();
        assert_eq!(
            try_resize_through_trait(&c, usize::MAX),
            Err(crate::SetMaxSizeError::CapacityOverflow)
        );
        assert_eq!(c.capacity(), 64);
    }
}
