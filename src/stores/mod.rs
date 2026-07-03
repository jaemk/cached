use crate::{Cached, CachedIter, CachedPeek, CachedRead};
use std::cmp::Eq;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::hash::Hash;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Default hash builder for non-sharded in-memory stores.
///
/// Resolves to `ahash::RandomState` when the `ahash` feature is enabled,
/// and to `std::collections::hash_map::RandomState` otherwise. This
/// matches the behavior prior to the introduction of the `S` type parameter,
/// so existing code that does not name the hasher is unaffected.
///
/// # Example
///
/// Use the default hasher (no change required for existing code):
///
/// ```rust
/// use cached::{Cached, UnboundCache};
///
/// let mut cache: UnboundCache<u32, u32> = UnboundCache::new();
/// cache.cache_set(1, 100);
/// assert_eq!(cache.cache_get(&1), Some(&100));
/// ```
///
/// Use a custom hasher via the builder's `.hasher()` method:
///
/// ```rust
/// use cached::{Cached, UnboundCache, DefaultHashBuilder};
/// use std::collections::hash_map::RandomState;
///
/// let mut cache = UnboundCache::<u32, u32>::builder()
///     .hasher(RandomState::new())
///     .build()
///     .unwrap();
/// cache.cache_set(1, 100);
/// assert_eq!(cache.cache_get(&1), Some(&100));
/// ```
#[cfg(feature = "ahash")]
pub type DefaultHashBuilder = ahash::RandomState;

/// Default hash builder for non-sharded in-memory stores.
///
/// Resolves to `ahash::RandomState` when the `ahash` feature is enabled,
/// and to `std::collections::hash_map::RandomState` otherwise. This
/// matches the behavior prior to the introduction of the `S` type parameter,
/// so existing code that does not name the hasher is unaffected.
#[cfg(not(feature = "ahash"))]
pub type DefaultHashBuilder = std::collections::hash_map::RandomState;

/// Construct a fresh [`DefaultHashBuilder`].
///
/// Abstracts over `ahash::RandomState::new()` vs `std::collections::hash_map::RandomState::new()`
/// since `ahash::RandomState` does not implement `Default`.
#[inline]
pub(super) fn new_default_hash_builder() -> DefaultHashBuilder {
    #[cfg(feature = "ahash")]
    {
        ahash::RandomState::new()
    }
    #[cfg(not(feature = "ahash"))]
    {
        std::collections::hash_map::RandomState::new()
    }
}

const STRIPE_COUNT: usize = 16;

#[repr(align(128))]
struct Slot(AtomicU64);

/// A hit/miss counter distributed across [`STRIPE_COUNT`] cache-line-padded
/// slots to reduce cross-core cache-line bouncing under concurrent increments.
///
/// Each thread is assigned a stable slot on first use via a thread-local index;
/// [`load`](StripedCounter::load) sums all slots for the aggregate value.
///
/// Used only by stores that implement [`CachedRead`], which allow concurrent
/// shared-lock reads. Stores that are always accessed under an exclusive write
/// lock use plain [`AtomicU64`] instead.
pub(super) struct StripedCounter {
    slots: Box<[Slot]>,
}

impl StripedCounter {
    pub(super) fn new() -> Self {
        let slots = (0..STRIPE_COUNT)
            .map(|_| Slot(AtomicU64::new(0)))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self { slots }
    }

    /// Increment the current thread's stripe by one.
    #[inline]
    pub(super) fn increment(&self) {
        self.slots[thread_stripe()]
            .0
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Sum across all stripes.
    pub(super) fn load(&self) -> u64 {
        self.slots.iter().map(|s| s.0.load(Ordering::Relaxed)).sum()
    }

    /// Zero all stripes (used by `cache_reset`).
    pub(super) fn reset(&self) {
        for slot in self.slots.iter() {
            slot.0.store(0, Ordering::Relaxed);
        }
    }

    /// Return a new `StripedCounter` whose slot 0 holds the current aggregate.
    /// Used by manual `Clone` impls that carry counter state across the copy.
    pub(super) fn snapshot(&self) -> Self {
        let total = self.load();
        let new = Self::new();
        new.slots[0].0.store(total, Ordering::Relaxed);
        new
    }
}

#[inline]
fn thread_stripe() -> usize {
    thread_local! {
        static SLOT: usize = {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            NEXT.fetch_add(1, Ordering::Relaxed) % STRIPE_COUNT
        };
    }
    SLOT.with(|&s| s)
}

#[cfg(feature = "async_core")]
use {super::CachedGetOrSetAsync, std::future::Future};

mod expiring;
mod expiring_lru;
mod lru;
#[cfg(feature = "time_stores")]
mod lru_ttl;
#[cfg(feature = "redb_store")]
mod redb;
#[cfg(feature = "redis_store")]
mod redis;
pub mod sharded;
#[cfg(feature = "time_stores")]
mod ttl;
#[cfg(feature = "time_stores")]
mod ttl_sorted;
mod unbound;

#[cfg(any(
    feature = "time_stores",
    feature = "redb_store",
    feature = "redis_store"
))]
use crate::time::Duration;
#[cfg(feature = "time_stores")]
use crate::time::Instant;

pub(super) type OnEvict<K, V> = std::sync::Arc<dyn Fn(&K, &V) + Send + Sync>;

/// Error returned by cache builder `build()` methods.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildError {
    /// A required field was not supplied to the builder.
    MissingRequired(&'static str),
    /// A field value is invalid.
    InvalidValue {
        /// The field whose value is invalid.
        field: &'static str,
        /// Human-readable reason.
        reason: &'static str,
    },
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BuildError::MissingRequired(field) => write!(f, "required field `{field}` was not set"),
            BuildError::InvalidValue { field, reason } => {
                write!(f, "invalid value for field `{field}`: {reason}")
            }
        }
    }
}

impl std::error::Error for BuildError {}

/// Error returned by `try_set_max_size` methods.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetMaxSizeError {
    /// A max size of zero was supplied; max_size must be greater than zero.
    ZeroSize,
}

impl std::fmt::Display for SetMaxSizeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SetMaxSizeError::ZeroSize => write!(f, "max_size must be greater than zero"),
        }
    }
}

impl std::error::Error for SetMaxSizeError {}

/// Error returned by [`CacheTtl::try_set_ttl`](crate::CacheTtl::try_set_ttl).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetTtlError {
    /// A TTL of zero was supplied; ttl must be greater than zero.
    ZeroTtl,
}

impl std::fmt::Display for SetTtlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SetTtlError::ZeroTtl => write!(f, "ttl must be greater than zero"),
        }
    }
}

impl std::error::Error for SetTtlError {}

/// Error returned by [`TtlCache`](crate::stores::TtlCache),
/// [`LruTtlCache`](crate::stores::LruTtlCache), and
/// [`TtlSortedCache`](crate::stores::TtlSortedCache) via [`Cached::cache_try_set`] when
/// an entry cannot be stored - currently only when computing the entry's expiry
/// `Instant` overflows.
///
/// The separate `TtlSortedCacheError` type was removed in favor of this unified type; the
/// following must not compile (guards against the old error type being reintroduced):
///
/// ```compile_fail
/// use cached::stores::TtlSortedCacheError;
/// ```
/// ```compile_fail
/// let _ = cached::TtlSortedCacheError::TimeBounds;
/// ```
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheSetError {
    /// Computing the entry's expiry `Instant` overflowed `Instant`'s representable range.
    TimeBounds,
}
impl std::fmt::Display for CacheSetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CacheSetError::TimeBounds => f.write_str("ttl is outside Instant bounds"),
        }
    }
}
impl std::error::Error for CacheSetError {}

/// Validate that `ttl` is non-zero; used by all TTL-capable store builders.
#[cfg(any(
    feature = "time_stores",
    feature = "redb_store",
    feature = "redis_store"
))]
pub(crate) fn validate_ttl(ttl: Duration) -> Result<(), BuildError> {
    if ttl.is_zero() {
        Err(BuildError::InvalidValue {
            field: "ttl",
            reason: "must be greater than zero",
        })
    } else {
        Ok(())
    }
}

/// A cached value paired with its per-entry expiry instant for TTL tracking.
///
/// Used internally by [`TtlCache`], [`LruTtlCache`], [`ShardedTtlCache`], and
/// [`ShardedLruTtlCache`]. The `expires_at` field holds the absolute instant at
/// which this entry expires, or `None` if the entry never expires (i.e. the TTL
/// was disabled at insert time via `set_ttl(Duration::ZERO)` / `unset_ttl()`).
///
/// Because expiry is fixed at insertion time, calling `set_ttl` after inserting an
/// entry does **not** retroactively change when existing entries expire. Only newly
/// inserted (or refreshed-on-hit) entries use the TTL current at that point.
#[cfg(feature = "time_stores")]
#[derive(Debug)]
pub(crate) struct TimedEntry<V> {
    /// The absolute instant at which this entry expires, or `None` for never.
    pub(crate) expires_at: Option<Instant>,
    /// The cached value.
    pub(crate) value: V,
}

#[cfg(feature = "time_stores")]
impl<V: Clone> Clone for TimedEntry<V> {
    fn clone(&self) -> Self {
        Self {
            expires_at: self.expires_at,
            value: self.value.clone(),
        }
    }
}

#[cfg(feature = "redb_store")]
#[cfg_attr(docsrs, doc(cfg(feature = "redb_store")))]
pub use crate::stores::redb::{RedbCache, RedbCacheBuildError, RedbCacheBuilder, RedbCacheError};
#[cfg(feature = "redis_store")]
#[cfg_attr(docsrs, doc(cfg(feature = "redis_store")))]
pub use crate::stores::redis::{
    ConnectionString, RedisCache, RedisCacheBuildError, RedisCacheBuilder, RedisCacheError,
};
pub use expiring::{ExpiringCache, ExpiringCacheBuilder};
pub use expiring_lru::{Expires, ExpiringLruCache, ExpiringLruCacheBuilder};
pub use lru::{LruCache, LruCacheBuilder};
#[cfg(feature = "time_stores")]
#[cfg_attr(docsrs, doc(cfg(feature = "time_stores")))]
pub use lru_ttl::{HasEvict, LruTtlCache, LruTtlCacheBuilder, NoEvict};
#[cfg(feature = "time_stores")]
#[cfg_attr(docsrs, doc(cfg(feature = "time_stores")))]
pub use ttl::{TtlCache, TtlCacheBuilder};
#[cfg(feature = "time_stores")]
#[cfg_attr(docsrs, doc(cfg(feature = "time_stores")))]
pub use ttl_sorted::{TtlSortedCache, TtlSortedCacheBuilder};
pub use unbound::{UnboundCache, UnboundCacheBuilder};

pub use sharded::{
    DefaultShardHasher, ShardHasher, ShardedExpiringCache, ShardedExpiringCacheBase,
    ShardedExpiringCacheBuilder, ShardedExpiringLruCache, ShardedExpiringLruCacheBase,
    ShardedExpiringLruCacheBuilder, ShardedLruCache, ShardedLruCacheBase, ShardedLruCacheBuilder,
    ShardedUnboundCache, ShardedUnboundCacheBase, ShardedUnboundCacheBuilder,
};
#[cfg(feature = "time_stores")]
#[cfg_attr(docsrs, doc(cfg(feature = "time_stores")))]
pub use sharded::{
    ShardedLruTtlCache, ShardedLruTtlCacheBase, ShardedLruTtlCacheBuilder, ShardedTtlCache,
    ShardedTtlCacheBase, ShardedTtlCacheBuilder,
};

// Canonical `AsyncRedisCache` availability gate (kept in sync with src/lib.rs and
// src/stores/redis.rs): a redis async runtime feature must be enabled. The six runtime features
// each imply `redis_store` + `async`; the capability-only features are excluded (no runtime).
#[cfg(any(
    feature = "redis_smol",
    feature = "redis_smol_native_tls",
    feature = "redis_smol_rustls",
    feature = "redis_tokio",
    feature = "redis_tokio_native_tls",
    feature = "redis_tokio_rustls",
))]
#[cfg_attr(
    docsrs,
    doc(cfg(any(
        feature = "redis_smol",
        feature = "redis_smol_native_tls",
        feature = "redis_smol_rustls",
        feature = "redis_tokio",
        feature = "redis_tokio_native_tls",
        feature = "redis_tokio_rustls",
    )))
)]
pub use crate::stores::redis::{AsyncRedisCache, AsyncRedisCacheBuilder};

impl<K, V, S> Cached<K, V> for HashMap<K, V, S>
where
    K: Hash + Eq,
    S: std::hash::BuildHasher,
{
    type Error = std::convert::Infallible;

    fn cache_get<Q>(&mut self, k: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        HashMap::get(self, k)
    }
    fn cache_get_mut<Q>(&mut self, k: &Q) -> Option<&mut V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        HashMap::get_mut(self, k)
    }
    fn cache_set(&mut self, k: K, v: V) -> Option<V> {
        HashMap::insert(self, k, v)
    }
    fn cache_get_or_set_with_mut<F: FnOnce() -> V>(&mut self, key: K, f: F) -> &mut V {
        self.entry(key).or_insert_with(f)
    }
    fn cache_try_get_or_set_with_mut<F: FnOnce() -> Result<V, E>, E>(
        &mut self,
        key: K,
        f: F,
    ) -> Result<&mut V, E> {
        let v = match self.entry(key) {
            Entry::Occupied(occupied) => occupied.into_mut(),
            Entry::Vacant(vacant) => vacant.insert(f()?),
        };

        Ok(v)
    }
    fn cache_remove<Q>(&mut self, k: &Q) -> Option<V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        HashMap::remove(self, k)
    }
    fn cache_remove_entry<Q>(&mut self, k: &Q) -> Option<(K, V)>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        HashMap::remove_entry(self, k)
    }
    fn cache_clear(&mut self) {
        HashMap::clear(self);
    }
    fn cache_reset(&mut self) {
        // Clear and release capacity without requiring `S: Default`, so stores built with a
        // non-`Default` `BuildHasher` (e.g. a seeded hasher, or ahash on wasm where
        // `RandomState: Default` is gated off) still implement `Cached`. The existing hasher
        // instance is preserved, matching the "configuration preserved across reset" contract.
        HashMap::clear(self);
        self.shrink_to_fit();
        self.cache_reset_metrics();
    }
    fn cache_size(&self) -> usize {
        HashMap::len(self)
    }
}

impl<K, V, S> CachedIter<K, V> for HashMap<K, V, S>
where
    K: Hash + Eq,
    S: std::hash::BuildHasher,
{
    fn iter<'a>(&'a self) -> impl Iterator<Item = (&'a K, &'a V)> + 'a
    where
        K: 'a,
        V: 'a,
    {
        self.iter()
    }
}

impl<K, V, S> CachedPeek<K, V> for HashMap<K, V, S>
where
    K: Hash + Eq,
    S: std::hash::BuildHasher,
{
    fn cache_peek<Q>(&self, k: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        HashMap::get(self, k)
    }
}

impl<K, V, S> CachedRead<K, V> for HashMap<K, V, S>
where
    K: Hash + Eq,
    S: std::hash::BuildHasher,
{
}

#[cfg(feature = "async_core")]
impl<K, V, S> CachedGetOrSetAsync<K, V> for HashMap<K, V, S>
where
    K: Hash + Eq + Clone + Send,
    S: std::hash::BuildHasher + Send,
{
    fn async_cache_get_or_set_with_mut<'a, F, Fut>(
        &'a mut self,
        k: K,
        f: F,
    ) -> impl Future<Output = &'a mut V> + Send + 'a
    where
        K: 'a,
        V: Send + 'a,
        F: FnOnce() -> Fut + Send + 'a,
        Fut: Future<Output = V> + Send + 'a,
    {
        async move {
            match self.entry(k) {
                Entry::Occupied(o) => o.into_mut(),
                Entry::Vacant(v) => v.insert(f().await),
            }
        }
    }

    fn async_cache_try_get_or_set_with_mut<'a, F, Fut, E>(
        &'a mut self,
        k: K,
        f: F,
    ) -> impl Future<Output = Result<&'a mut V, E>> + Send + 'a
    where
        K: 'a,
        V: Send + 'a,
        E: 'a,
        F: FnOnce() -> Fut + Send + 'a,
        Fut: Future<Output = Result<V, E>> + Send + 'a,
    {
        async move {
            let v = match self.entry(k) {
                Entry::Occupied(o) => o.into_mut(),
                Entry::Vacant(v) => v.insert(f().await?),
            };
            Ok(v)
        }
    }
}

/// Trait for cache stores that support explicit eviction of expired entries.
///
/// Implementors remove all entries that are past their expiry from the store and
/// invoke the `on_evict` callback (if configured) for each removed entry.
///
/// `evict()` is the explicit way to physically remove expired entries, reclaim
/// memory, and obtain an accurate live count. After calling `evict()`, `len()`
/// reflects only live entries. Without it, `len()` may count expired-but-not-yet-swept
/// entries while `iter().count()` omits them - the two can differ on any lazy-eviction
/// store.
///
/// This trait is for in-memory stores with infallible expiration checks. IO-backed
/// stores expose their own APIs because sweeping can fail: `RedbCache` uses
/// `remove_expired_entries`, while Redis relies on server-side key expiry.
pub trait CacheEvict {
    /// Physically remove all expired entries from the cache and return the count removed.
    ///
    /// After this call, `len()` reflects only live entries. Fires the `on_evict` callback
    /// and increments `cache_evictions()` for each removed entry. Hit/miss metrics are not
    /// affected; call [`cache_reset_metrics`](crate::Cached::cache_reset_metrics) separately
    /// if needed.
    ///
    /// **Note for sharded in-memory stores**: these are internally synchronized and normally held
    /// behind an `Arc`/`static`, so they cannot offer `&mut self`. They implement
    /// [`ConcurrentCacheEvict`] (a `&self` counterpart of this trait) instead, and also expose an
    /// inherent `evict(&self)` method.
    #[must_use]
    fn evict(&mut self) -> usize;
}

/// Concurrent counterpart of [`CacheEvict`] for internally-synchronized stores.
///
/// Sharded in-memory stores are normally held behind an `Arc`/`static`, so they
/// cannot offer the `&mut self` [`CacheEvict::evict`]. This trait provides the same
/// operation through a shared reference.
///
/// `evict()` is the explicit way to physically remove expired entries, reclaim
/// memory, and obtain an accurate live count on the sharded expiry-capable stores
/// (`ShardedTtlCache`, `ShardedLruTtlCache`, `ShardedExpiringCache`,
/// `ShardedExpiringLruCache`). After calling `evict()`, `len()` (the inherent method)
/// reflects only live entries.
pub trait ConcurrentCacheEvict {
    /// Physically remove all expired entries across all shards and return the count removed.
    ///
    /// After this call, `len()` (the inherent method on sharded stores) reflects only live
    /// entries. Fires `on_evict` and increments `cache_evictions()` for each removed entry.
    #[must_use]
    fn evict(&self) -> usize;
}

#[cfg(test)]
/// Cache store tests
mod tests {
    use super::*;

    #[test]
    fn hashmap() {
        let mut c = std::collections::HashMap::new();
        assert!(c.cache_get(&1).is_none());
        assert_eq!(c.cache_misses(), None);

        assert_eq!(c.cache_set(1, 100), None);
        assert_eq!(c.cache_get(&1), Some(&100));
        assert_eq!(c.cache_hits(), None);
        assert_eq!(c.cache_misses(), None);
    }

    #[test]
    fn build_error_display() {
        let err1 = BuildError::MissingRequired("ttl");
        assert_eq!(err1.to_string(), "required field `ttl` was not set");

        let err2 = BuildError::InvalidValue {
            field: "max_size",
            reason: "must be greater than zero",
        };
        assert_eq!(
            err2.to_string(),
            "invalid value for field `max_size`: must be greater than zero"
        );
    }

    #[test]
    fn cache_set_error_is_clone_eq() {
        // Parity with `SetMaxSizeError`/`SetTtlError`, which already derive these.
        assert_eq!(CacheSetError::TimeBounds, CacheSetError::TimeBounds.clone());
    }

    // Compile-time assertion: BuildError must implement Clone + PartialEq + Eq.
    fn _assert_build_error_bounds<T: Clone + PartialEq + Eq>() {}
    fn _check_build_error() {
        _assert_build_error_bounds::<BuildError>();
    }

    #[test]
    fn build_error_clone_partial_eq_eq() {
        // Two equal MissingRequired values compare equal and survive a clone round-trip.
        let a = BuildError::MissingRequired("ttl");
        let b = a.clone();
        assert_eq!(a, b);

        // Two equal InvalidValue values compare equal and survive a clone round-trip.
        let c = BuildError::InvalidValue {
            field: "max_size",
            reason: "must be greater than zero",
        };
        let d = c.clone();
        assert_eq!(c, d);

        // Different variants are not equal.
        assert_ne!(
            BuildError::MissingRequired("ttl"),
            BuildError::InvalidValue {
                field: "ttl",
                reason: "must be greater than zero",
            },
        );

        // Different field names inside the same variant are not equal.
        assert_ne!(
            BuildError::MissingRequired("ttl"),
            BuildError::MissingRequired("max_size"),
        );
    }

    // The author's `build_error_clone_partial_eq_eq` exercises clone/eq on both
    // variants, a cross-variant `assert_ne!`, and differing fields *within*
    // `MissingRequired`. The remaining derived-PartialEq paths for the struct
    // variant `InvalidValue` were not checked: equality must depend on BOTH the
    // `field` and the `reason` field. These would not fail to compile if the
    // derive were reverted (the wrapper enums prove `BuildError` already had a
    // hand-rolled `Debug`), so each comparison below is value-level and bites if
    // the `PartialEq`/`Eq`/`Clone` derive is removed.
    #[test]
    fn build_error_invalid_value_field_discriminates() {
        // Same `reason`, different `field` => not equal.
        assert_ne!(
            BuildError::InvalidValue {
                field: "max_size",
                reason: "must be greater than zero",
            },
            BuildError::InvalidValue {
                field: "ttl",
                reason: "must be greater than zero",
            },
        );

        // Same `field`, different `reason` => not equal.
        assert_ne!(
            BuildError::InvalidValue {
                field: "max_size",
                reason: "must be greater than zero",
            },
            BuildError::InvalidValue {
                field: "max_size",
                reason: "allocation failed",
            },
        );

        // Fully equal struct variants compare equal (and the clone matches).
        let a = BuildError::InvalidValue {
            field: "max_size",
            reason: "must be greater than zero",
        };
        assert_eq!(a, a.clone());
    }

    // The consumer-facing point of the derive: a real builder failure can be
    // compared with `assert_eq!`/matched, and cloned. `LruCacheBuilder::build`
    // is feature-free and produces both `BuildError` variants directly.
    #[test]
    fn lru_build_error_is_comparable_and_cloneable() {
        // Missing required field.
        let missing = LruCache::<i32, i32>::builder().build().unwrap_err();
        assert_eq!(missing, BuildError::MissingRequired("max_size"));
        // assert_eq! over a clone of a real builder error.
        assert_eq!(missing, missing.clone());

        // Invalid value (zero capacity).
        let invalid = LruCache::<i32, i32>::builder()
            .max_size(0)
            .build()
            .unwrap_err();
        assert_eq!(
            invalid,
            BuildError::InvalidValue {
                field: "max_size",
                reason: "must be greater than zero",
            }
        );

        // The two real failures are distinct.
        assert_ne!(missing, invalid);
    }

    // Wrapper enums (RedisCacheBuildError / RedbCacheBuildError) INTENTIONALLY do
    // NOT derive Clone/PartialEq/Eq (their other variants wrap non-Clone/Eq
    // errors). Assert only that the config path still surfaces the embedded
    // `BuildError` and that Debug/Display work. These tests need a live service
    // only past the field validation, which the builders run first, so no
    // network/disk is touched.
    #[cfg(feature = "redis_store")]
    #[test]
    fn redis_build_error_wraps_build_error_without_clone_eq() {
        // No prefix set => Build(BuildError::MissingRequired("prefix")) before any IO.
        let err = RedisCacheBuilder::<String, u32>::new().build().unwrap_err();
        assert!(
            matches!(
                err,
                RedisCacheBuildError::Build(BuildError::MissingRequired("prefix"))
            ),
            "expected Build(MissingRequired(\"prefix\")), got {err:?}"
        );
        // Debug and Display are available on the wrapper (transparent to BuildError).
        assert!(!format!("{err:?}").is_empty());
        assert_eq!(
            err.to_string(),
            BuildError::MissingRequired("prefix").to_string()
        );
    }

    #[cfg(feature = "redb_store")]
    #[test]
    fn redb_build_error_wraps_build_error_without_clone_eq() {
        // No name set => Build(BuildError::MissingRequired("name")) before any IO.
        let err = RedbCacheBuilder::<String, u32>::new().build().unwrap_err();
        assert!(
            matches!(
                err,
                RedbCacheBuildError::Build(BuildError::MissingRequired("name"))
            ),
            "expected Build(MissingRequired(\"name\")), got {err:?}"
        );
        assert!(!format!("{err:?}").is_empty());
        assert_eq!(
            err.to_string(),
            BuildError::MissingRequired("name").to_string()
        );
    }
}
