use crate::{Cached, CachedIter, CachedPeek, CachedRead};
use std::cmp::Eq;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::hash::Hash;

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
#[cfg(feature = "time_stores")]
mod ttl;
#[cfg(feature = "time_stores")]
mod ttl_sorted;
mod unbound;

use crate::time::Instant;

pub(super) type OnEvict<K, V> = std::sync::Arc<dyn Fn(&K, &V) + Send + Sync>;

/// Error returned by cache builder `try_build()` methods.
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
            field: "size",
            reason: "must be greater than zero",
        };
        assert_eq!(
            err2.to_string(),
            "invalid value for field `size`: must be greater than zero"
        );
    }
}
