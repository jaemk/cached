use crate::{Cached, CachedIter, CachedPeek, CachedRead};
use std::cmp::Eq;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::hash::Hash;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

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
use {super::CachedAsync, std::future::Future};

#[cfg(feature = "disk_store")]
mod disk;
mod expiring;
mod expiring_lru;
mod lru;
#[cfg(feature = "time_stores")]
mod lru_ttl;
#[cfg(feature = "redis_store")]
mod redis;
pub mod sharded;
#[cfg(feature = "time_stores")]
mod ttl;
#[cfg(feature = "time_stores")]
mod ttl_sorted;
mod unbound;

use crate::time::{Duration, Instant};

pub(super) type OnEvict<K, V> = std::sync::Arc<dyn Fn(&K, &V) + Send + Sync>;

/// Error returned by cache builder `build()` methods.
#[derive(Debug)]
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
    /// A zero TTL was supplied; TTL must be greater than zero.
    InvalidTtl {
        /// The invalid TTL value.
        ttl: Duration,
    },
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BuildError::MissingRequired(field) => write!(f, "required field `{field}` was not set"),
            BuildError::InvalidValue { field, reason } => {
                write!(f, "invalid value for field `{field}`: {reason}")
            }
            BuildError::InvalidTtl { ttl } => {
                write!(f, "invalid ttl {ttl:?}: must be greater than zero")
            }
        }
    }
}

impl std::error::Error for BuildError {}

/// Validate that `ttl` is non-zero; used by all TTL-capable store builders.
pub(crate) fn validate_ttl(ttl: Duration) -> Result<(), BuildError> {
    if ttl.is_zero() {
        Err(BuildError::InvalidTtl { ttl })
    } else {
        Ok(())
    }
}

/// A cached value paired with its insertion timestamp for TTL tracking.
///
/// Exposed through `TtlCache::store` and `LruTtlCache::store` for
/// advanced introspection of cache internals.
#[derive(Debug)]
pub struct TimedEntry<V> {
    /// The instant this entry was inserted (or last refreshed).
    pub instant: Instant,
    /// The cached value.
    pub value: V,
}

impl<V: Clone> Clone for TimedEntry<V> {
    fn clone(&self) -> Self {
        Self {
            instant: self.instant,
            value: self.value.clone(),
        }
    }
}

#[cfg(feature = "disk_store")]
pub use crate::stores::disk::{DiskCache, DiskCacheBuildError, DiskCacheBuilder, DiskCacheError};
#[cfg(feature = "redis_store")]
#[cfg_attr(docsrs, doc(cfg(feature = "redis_store")))]
pub use crate::stores::redis::{
    RedisCache, RedisCacheBuildError, RedisCacheBuilder, RedisCacheError,
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
pub use ttl_sorted::{TtlSortedCache, TtlSortedCacheBuilder, TtlSortedCacheError};
pub use unbound::{UnboundCache, UnboundCacheBuilder};

pub use sharded::{
    DefaultShardHasher, ShardHasher, ShardedCache, ShardedCacheBase, ShardedCacheBuilder,
    ShardedExpiringCache, ShardedExpiringCacheBase, ShardedExpiringCacheBuilder,
    ShardedExpiringLruCache, ShardedExpiringLruCacheBase, ShardedExpiringLruCacheBuilder,
    ShardedLruCache, ShardedLruCacheBase, ShardedLruCacheBuilder,
};
#[cfg(feature = "time_stores")]
#[cfg_attr(docsrs, doc(cfg(feature = "time_stores")))]
pub use sharded::{
    ShardedLruTtlCache, ShardedLruTtlCacheBase, ShardedLruTtlCacheBuilder, ShardedTtlCache,
    ShardedTtlCacheBase, ShardedTtlCacheBuilder,
};

#[cfg(all(
    feature = "async_core",
    feature = "redis_store",
    any(feature = "redis_smol", feature = "redis_tokio")
))]
#[cfg_attr(
    docsrs,
    doc(cfg(all(
        feature = "async_core",
        feature = "redis_store",
        any(feature = "redis_smol", feature = "redis_tokio")
    )))
)]
pub use crate::stores::redis::{AsyncRedisCache, AsyncRedisCacheBuilder};

impl<K, V, S> Cached<K, V> for HashMap<K, V, S>
where
    K: Hash + Eq,
    S: std::hash::BuildHasher + Default,
{
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
    fn cache_get_or_set_with<F: FnOnce() -> V>(&mut self, key: K, f: F) -> &mut V {
        self.entry(key).or_insert_with(f)
    }
    fn cache_try_get_or_set_with<F: FnOnce() -> Result<V, E>, E>(
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
        *self = HashMap::default();
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
impl<K, V, S> CachedAsync<K, V> for HashMap<K, V, S>
where
    K: Hash + Eq + Clone + Send,
    S: std::hash::BuildHasher + Send,
{
    fn async_get_or_set_with<'a, F, Fut>(
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

    fn async_try_get_or_set_with<'a, F, Fut, E>(
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
/// This trait is for in-memory stores with infallible expiration checks. IO-backed
/// stores expose their own APIs because sweeping can fail: `DiskCache` uses
/// `remove_expired_entries`, while Redis relies on server-side key expiry.
pub trait CacheEvict {
    /// Remove all expired entries from the cache, returning the number removed.
    ///
    /// Fires the `on_evict` callback and increments `cache_evictions()` for each removed entry.
    /// Hit/miss metrics are not affected; call [`cache_reset_metrics`](crate::Cached::cache_reset_metrics)
    /// separately if needed.
    ///
    /// **Note for sharded in-memory stores**: the concrete types expose an inherent `evict(&self)`
    /// method that does not require `&mut self`. When you hold only a shared reference (e.g., via
    /// `Arc`), call the inherent method directly instead of going through this trait.
    fn evict(&mut self) -> usize;
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
}
