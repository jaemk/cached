use super::{CacheEvict, Cached, Expires, UnboundCache};
use crate::{CachedIter, CachedPeek, CloneCached};
use std::hash::Hash;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[cfg(feature = "async_core")]
use {super::CachedAsync, std::collections::hash_map::Entry, std::future::Future};

/// Size-unbounded cache where each value controls its own expiry via [`Expires`].
///
/// Unlike [`TtlCache`](crate::stores::TtlCache) which applies a single global TTL duration to
/// all entries, `ExpiringCache` has **no global TTL**. Each value determines its own expiration
/// by implementing [`Expires`]. The store checks `is_expired()` on every lookup and evicts
/// expired entries on access.
///
/// For a size-bounded variant that also evicts by LRU, see [`ExpiringLruCache`](crate::ExpiringLruCache).
/// When using the `#[cached]` proc macro, `expires = true` automatically selects this store
/// (or `ExpiringLruCache` when `size` is also specified).
///
/// ## Memory note
///
/// `ExpiringCache` is **unbounded** and only removes expired entries when the same key is
/// accessed again. Entries that expire and are never re-fetched stay in memory indefinitely.
/// For high-cardinality workloads, call [`evict()`](ExpiringCache::evict) periodically to
/// sweep and remove all expired entries, or prefer [`ExpiringLruCache`](crate::ExpiringLruCache)
/// with a `size` bound to cap memory usage automatically.
///
/// ```rust
/// use cached::{Cached, Expires, ExpiringCache};
///
/// struct Token {
///     #[allow(dead_code)]
///     value: String,
///     expired: bool,
/// }
/// impl Expires for Token {
///     fn is_expired(&self) -> bool { self.expired }
/// }
///
/// let mut cache: ExpiringCache<u32, Token> = ExpiringCache::new();
/// cache.cache_set(1, Token { value: "live".into(), expired: false });
/// assert!(cache.cache_get(&1).is_some());
/// cache.cache_set(2, Token { value: "stale".into(), expired: true });
/// assert!(cache.cache_get(&2).is_none()); // expired → not returned
/// ```
///
/// Note: This cache is in-memory only.
pub struct ExpiringCache<K: Hash + Eq, V: Expires> {
    pub(super) store: UnboundCache<K, V>,
    pub(super) hits: AtomicU64,
    pub(super) misses: AtomicU64,
    pub(super) evictions: AtomicU64,
    pub(super) on_evict: Option<super::OnEvict<K, V>>,
}

impl<K: Hash + Eq, V: Expires> std::fmt::Debug for ExpiringCache<K, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExpiringCache")
            .field("hits", &self.hits.load(Ordering::Relaxed))
            .field("misses", &self.misses.load(Ordering::Relaxed))
            .field("evictions", &self.evictions.load(Ordering::Relaxed))
            .field("on_evict", &self.on_evict.as_ref().map(|_| "on_evict"))
            .finish()
    }
}

impl<K, V> Clone for ExpiringCache<K, V>
where
    K: Clone + Hash + Eq,
    V: Expires + Clone,
{
    fn clone(&self) -> Self {
        Self {
            store: self.store.clone(),
            hits: AtomicU64::new(self.hits.load(Ordering::Relaxed)),
            misses: AtomicU64::new(self.misses.load(Ordering::Relaxed)),
            evictions: AtomicU64::new(self.evictions.load(Ordering::Relaxed)),
            on_evict: self.on_evict.clone(),
        }
    }
}

/// Builder for [`ExpiringCache`].
pub struct ExpiringCacheBuilder<K, V: Expires> {
    capacity: Option<usize>,
    on_evict: Option<super::OnEvict<K, V>>,
}

impl<K, V: Expires> Default for ExpiringCacheBuilder<K, V> {
    fn default() -> Self {
        Self {
            capacity: None,
            on_evict: None,
        }
    }
}

impl<K, V: Expires> ExpiringCacheBuilder<K, V> {
    /// Set the initial allocation capacity (optional).
    #[must_use]
    pub fn capacity(mut self, capacity: usize) -> Self {
        self.capacity = Some(capacity);
        self
    }

    /// Set a callback to be invoked when an expired entry is removed from the cache.
    ///
    /// The callback fires when an expired value is encountered during `cache_get`,
    /// `cache_get_mut`, `cache_get_or_set_with`, `cache_try_get_or_set_with`,
    /// their async equivalents, or an explicit `evict()` sweep.
    /// It does **not** fire on `cache_remove` or `cache_clear` (consistent with
    /// [`ExpiringLruCache`](crate::ExpiringLruCache)).
    #[must_use]
    pub fn on_evict(mut self, on_evict: impl Fn(&K, &V) + Send + Sync + 'static) -> Self {
        self.on_evict = Some(Arc::new(on_evict));
        self
    }

    /// Build the cache.
    #[must_use]
    pub fn build(self) -> ExpiringCache<K, V>
    where
        K: Hash + Eq,
    {
        let mut cache = match self.capacity {
            Some(cap) => ExpiringCache::with_capacity(cap),
            None => ExpiringCache::new(),
        };
        cache.on_evict = self.on_evict;
        cache
    }

    /// Build the cache, returning an error instead of panicking.
    ///
    /// `ExpiringCache` has no required fields, so this always succeeds.
    /// Provided for API consistency with other builders.
    ///
    /// # Errors
    ///
    /// This method currently never returns an error.
    pub fn try_build(self) -> Result<ExpiringCache<K, V>, super::BuildError>
    where
        K: Hash + Eq,
    {
        Ok(self.build())
    }
}

impl<K: Hash + Eq, V: Expires> ExpiringCache<K, V> {
    /// Return a builder for constructing an [`ExpiringCache`].
    #[must_use]
    pub fn builder() -> ExpiringCacheBuilder<K, V> {
        ExpiringCacheBuilder::default()
    }

    /// Creates an empty `ExpiringCache`
    #[must_use]
    pub fn new() -> ExpiringCache<K, V> {
        ExpiringCache {
            store: UnboundCache::new(),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            evictions: AtomicU64::new(0),
            on_evict: None,
        }
    }

    /// Creates an empty `ExpiringCache` with a given pre-allocated capacity
    #[must_use]
    pub fn with_capacity(capacity: usize) -> ExpiringCache<K, V> {
        ExpiringCache {
            store: UnboundCache::with_capacity(capacity),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            evictions: AtomicU64::new(0),
            on_evict: None,
        }
    }

    /// Evict all expired entries from the cache.
    ///
    /// Returns the number of entries removed. Fires the `on_evict` callback for each
    /// removed entry. Use this periodically for high-cardinality workloads to reclaim
    /// memory from entries that expire but are never re-accessed.
    pub fn evict(&mut self) -> usize {
        let on_evict = &self.on_evict;
        let evictions = &self.evictions;
        let mut removed = 0;
        self.store.store.retain(|key, value| {
            if value.is_expired() {
                if let Some(on_evict) = on_evict {
                    on_evict(key, value);
                }
                evictions.fetch_add(1, Ordering::Relaxed);
                removed += 1;
                false
            } else {
                true
            }
        });
        removed
    }
}

impl<K: Hash + Eq, V: Expires> Default for ExpiringCache<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Hash + Eq, V: Expires> Cached<K, V> for ExpiringCache<K, V> {
    fn cache_get<Q>(&mut self, k: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        // Two lookups on the hit path: the first checks expiry (releasing the borrow via
        // `.map`), the second returns the reference. A single-lookup approach is not possible
        // in stable Rust because returning `&'1 V` from inside an `if let` block ties the
        // borrow to lifetime `'1`, which prevents `remove_entry` (a mutable borrow) even on
        // the non-returning path. Polonius (nightly) would fix this.
        match self.store.store.get(k).map(|v| v.is_expired()) {
            None => {
                self.misses.fetch_add(1, Ordering::Relaxed);
                None
            }
            Some(true) => {
                self.misses.fetch_add(1, Ordering::Relaxed);
                if let Some((key, old)) = self.store.store.remove_entry(k) {
                    if let Some(on_evict) = &self.on_evict {
                        on_evict(&key, &old);
                    }
                    self.evictions.fetch_add(1, Ordering::Relaxed);
                }
                None
            }
            Some(false) => {
                self.hits.fetch_add(1, Ordering::Relaxed);
                self.store.store.get(k)
            }
        }
    }

    fn cache_get_mut<Q>(&mut self, k: &Q) -> Option<&mut V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        // Two lookups on the hit path for the same reason as `cache_get` (NLL limitation).
        match self.store.store.get(k).map(|v| v.is_expired()) {
            None => {
                self.misses.fetch_add(1, Ordering::Relaxed);
                None
            }
            Some(true) => {
                self.misses.fetch_add(1, Ordering::Relaxed);
                if let Some((key, old)) = self.store.store.remove_entry(k) {
                    if let Some(on_evict) = &self.on_evict {
                        on_evict(&key, &old);
                    }
                    self.evictions.fetch_add(1, Ordering::Relaxed);
                }
                None
            }
            Some(false) => {
                self.hits.fetch_add(1, Ordering::Relaxed);
                self.store.store.get_mut(k)
            }
        }
    }

    fn cache_get_or_set_with<F: FnOnce() -> V>(&mut self, k: K, f: F) -> &mut V {
        match self.store.store.entry(k) {
            std::collections::hash_map::Entry::Occupied(mut occupied) => {
                if !occupied.get().is_expired() {
                    self.hits.fetch_add(1, Ordering::Relaxed);
                    occupied.into_mut()
                } else {
                    self.misses.fetch_add(1, Ordering::Relaxed);
                    let old = occupied.insert(f());
                    if let Some(on_evict) = &self.on_evict {
                        on_evict(occupied.key(), &old);
                    }
                    self.evictions.fetch_add(1, Ordering::Relaxed);
                    occupied.into_mut()
                }
            }
            std::collections::hash_map::Entry::Vacant(vacant) => {
                self.misses.fetch_add(1, Ordering::Relaxed);
                vacant.insert(f())
            }
        }
    }

    fn cache_try_get_or_set_with<F: FnOnce() -> Result<V, E>, E>(
        &mut self,
        k: K,
        f: F,
    ) -> Result<&mut V, E> {
        match self.store.store.entry(k) {
            std::collections::hash_map::Entry::Occupied(mut occupied) => {
                if !occupied.get().is_expired() {
                    self.hits.fetch_add(1, Ordering::Relaxed);
                    Ok(occupied.into_mut())
                } else {
                    self.misses.fetch_add(1, Ordering::Relaxed);
                    let new_val = f()?;
                    let old = occupied.insert(new_val);
                    if let Some(on_evict) = &self.on_evict {
                        on_evict(occupied.key(), &old);
                    }
                    self.evictions.fetch_add(1, Ordering::Relaxed);
                    Ok(occupied.into_mut())
                }
            }
            std::collections::hash_map::Entry::Vacant(vacant) => {
                self.misses.fetch_add(1, Ordering::Relaxed);
                Ok(vacant.insert(f()?))
            }
        }
    }

    fn cache_set(&mut self, k: K, v: V) -> Option<V> {
        self.store.cache_set(k, v)
    }

    fn cache_remove<Q>(&mut self, k: &Q) -> Option<V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        self.store.cache_remove(k)
    }

    fn cache_clear(&mut self) {
        self.store.cache_clear();
    }

    fn cache_reset(&mut self) {
        self.store.cache_reset();
        self.cache_reset_metrics();
    }

    fn cache_size(&self) -> usize {
        self.store.cache_size()
    }

    fn cache_capacity(&self) -> Option<usize> {
        None
    }

    fn cache_hits(&self) -> Option<u64> {
        Some(self.hits.load(Ordering::Relaxed))
    }

    fn cache_misses(&self) -> Option<u64> {
        Some(self.misses.load(Ordering::Relaxed))
    }

    fn cache_evictions(&self) -> Option<u64> {
        Some(self.evictions.load(Ordering::Relaxed))
    }

    fn cache_reset_metrics(&mut self) {
        self.hits.store(0, Ordering::Relaxed);
        self.misses.store(0, Ordering::Relaxed);
        self.evictions.store(0, Ordering::Relaxed);
        self.store.cache_reset_metrics();
    }
}

impl<K: Hash + Eq, V: Expires> CachedIter<K, V> for ExpiringCache<K, V> {
    fn iter<'a>(&'a self) -> impl Iterator<Item = (&'a K, &'a V)> + 'a
    where
        K: 'a,
        V: 'a,
    {
        self.store
            .store
            .iter()
            .filter_map(|(k, v)| if v.is_expired() { None } else { Some((k, v)) })
    }
}

impl<K: Hash + Eq, V: Expires> CachedPeek<K, V> for ExpiringCache<K, V> {
    fn cache_peek<Q>(&self, key: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        self.store.store.get(key).and_then(|value| {
            if value.is_expired() {
                None
            } else {
                Some(value)
            }
        })
    }
}

#[cfg(feature = "async_core")]
impl<K, V> CachedAsync<K, V> for ExpiringCache<K, V>
where
    K: Hash + Eq + Send,
    V: Expires + Send,
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
            match self.store.store.entry(k) {
                Entry::Occupied(mut occupied) => {
                    if !occupied.get().is_expired() {
                        self.hits.fetch_add(1, Ordering::Relaxed);
                        occupied.into_mut()
                    } else {
                        self.misses.fetch_add(1, Ordering::Relaxed);
                        let old = occupied.insert(f().await);
                        if let Some(on_evict) = &self.on_evict {
                            on_evict(occupied.key(), &old);
                        }
                        self.evictions.fetch_add(1, Ordering::Relaxed);
                        occupied.into_mut()
                    }
                }
                Entry::Vacant(vacant) => {
                    self.misses.fetch_add(1, Ordering::Relaxed);
                    vacant.insert(f().await)
                }
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
            let v = match self.store.store.entry(k) {
                Entry::Occupied(mut occupied) => {
                    if !occupied.get().is_expired() {
                        self.hits.fetch_add(1, Ordering::Relaxed);
                        occupied.into_mut()
                    } else {
                        self.misses.fetch_add(1, Ordering::Relaxed);
                        let new_val = f().await?;
                        let old = occupied.insert(new_val);
                        if let Some(on_evict) = &self.on_evict {
                            on_evict(occupied.key(), &old);
                        }
                        self.evictions.fetch_add(1, Ordering::Relaxed);
                        occupied.into_mut()
                    }
                }
                Entry::Vacant(vacant) => {
                    self.misses.fetch_add(1, Ordering::Relaxed);
                    vacant.insert(f().await?)
                }
            };
            Ok(v)
        }
    }
}

impl<K: Hash + Eq, V: Expires + Clone> CloneCached<K, V> for ExpiringCache<K, V> {
    // Unlike `cache_get`, this intentionally leaves an expired entry in the map so the
    // `result_fallback` path can clone and return it as a stale-but-present value on `Err`.
    // The entry remains visible via `cache_size()` and `CachedIter` until the next
    // `cache_get`, `evict()`, or an explicit `cache_remove`.
    fn cache_get_with_expiry_status<Q>(&mut self, k: &Q) -> (Option<V>, bool)
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        if let Some(value) = self.store.store.get(k) {
            let expired = value.is_expired();
            if expired {
                self.misses.fetch_add(1, Ordering::Relaxed);
                (Some(value.clone()), true)
            } else {
                self.hits.fetch_add(1, Ordering::Relaxed);
                (Some(value.clone()), false)
            }
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
            (None, false)
        }
    }
}

impl<K: std::hash::Hash + Eq, V: Expires> CacheEvict for ExpiringCache<K, V> {
    fn evict(&mut self) -> usize {
        ExpiringCache::evict(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct ExpiredU8(pub u8);

    impl Expires for ExpiredU8 {
        fn is_expired(&self) -> bool {
            self.0 > 10
        }
    }

    #[test]
    fn expiring_cache_get_miss() {
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::new();
        assert!(c.get(&1).is_none());
        assert_eq!(c.cache_hits(), Some(0));
        assert_eq!(c.cache_misses(), Some(1));
    }

    #[test]
    fn expiring_cache_get_hit() {
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::new();
        assert!(c.set(1, ExpiredU8(2)).is_none());
        assert_eq!(c.get(&1), Some(&ExpiredU8(2)));
        assert_eq!(c.cache_hits(), Some(1));
        assert_eq!(c.cache_misses(), Some(0));
    }

    #[test]
    fn expiring_cache_get_expired() {
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::new();
        assert!(c.set(2, ExpiredU8(12)).is_none());
        assert!(c.get(&2).is_none());
        assert_eq!(c.cache_hits(), Some(0));
        assert_eq!(c.cache_misses(), Some(1));
        assert_eq!(c.cache_evictions(), Some(1));
    }

    #[test]
    fn expiring_cache_builder() {
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder()
            .capacity(10)
            .on_evict(|_k: &u8, v: &ExpiredU8| {
                assert!(v.0 > 10);
            })
            .build();
        assert!(c.set(1, ExpiredU8(15)).is_none());
        assert!(c.get(&1).is_none());
        assert_eq!(c.cache_evictions(), Some(1));
    }

    #[test]
    fn expiring_cache_evict_fires_callback() {
        use std::sync::{Arc, Mutex};
        let fired: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(vec![]));
        let fired2 = fired.clone();
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder()
            .on_evict(move |k: &u8, _v: &ExpiredU8| {
                fired2.lock().unwrap().push(*k);
            })
            .build();
        c.set(1, ExpiredU8(15)); // expired
        c.set(2, ExpiredU8(3)); // live
        let n = c.evict();
        assert_eq!(n, 1);
        assert_eq!(c.cache_evictions(), Some(1));
        let mut keys = fired.lock().unwrap().clone();
        keys.sort();
        assert_eq!(keys, vec![1]);
        assert_eq!(c.cache_size(), 1);
    }

    #[test]
    fn expiring_cache_remove_does_not_fire_on_evict() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering as AOrdering},
            Arc,
        };
        let count = Arc::new(AtomicUsize::new(0));
        let count2 = count.clone();
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder()
            .on_evict(move |_k: &u8, _v: &ExpiredU8| {
                count2.fetch_add(1, AOrdering::Relaxed);
            })
            .build();
        c.set(1, ExpiredU8(5)); // live
        assert_eq!(c.cache_remove(&1), Some(ExpiredU8(5)));
        assert_eq!(
            count.load(AOrdering::Relaxed),
            0,
            "on_evict must not fire on cache_remove"
        );
        assert_eq!(c.cache_evictions(), Some(0));
    }

    #[test]
    fn expiring_cache_get_mut_hit() {
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::new();
        c.set(1, ExpiredU8(2));
        let v = c.cache_get_mut(&1).expect("should be a cache hit");
        assert_eq!(*v, ExpiredU8(2));
        assert_eq!(c.cache_hits(), Some(1));
        assert_eq!(c.cache_misses(), Some(0));
    }

    #[test]
    fn expiring_cache_get_mut_expired() {
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::new();
        c.set(1, ExpiredU8(15)); // expired
        assert!(c.cache_get_mut(&1).is_none());
        assert_eq!(c.cache_hits(), Some(0));
        assert_eq!(c.cache_misses(), Some(1));
        assert_eq!(c.cache_evictions(), Some(1));
        assert_eq!(c.cache_size(), 0);
    }

    #[test]
    fn expiring_cache_get_or_set_with_hit_no_closure() {
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::new();
        c.set(1, ExpiredU8(5));
        let mut called = false;
        let v = c.cache_get_or_set_with(1, || {
            called = true;
            ExpiredU8(99)
        });
        assert!(!called, "closure must not be called on cache hit");
        assert_eq!(*v, ExpiredU8(5));
        assert_eq!(c.cache_hits(), Some(1));
    }

    #[test]
    fn expiring_cache_get_or_set_with_expired_fires_on_evict() {
        use std::sync::{Arc, Mutex};
        let fired: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(vec![]));
        let fired2 = fired.clone();
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder()
            .on_evict(move |k: &u8, _v: &ExpiredU8| {
                fired2.lock().unwrap().push(*k);
            })
            .build();
        c.set(1, ExpiredU8(15)); // expired
        let v = c.cache_get_or_set_with(1, || ExpiredU8(3));
        assert_eq!(*v, ExpiredU8(3));
        assert_eq!(c.cache_misses(), Some(1));
        assert_eq!(c.cache_evictions(), Some(1));
        assert_eq!(fired.lock().unwrap().clone(), vec![1]);
    }

    #[test]
    fn expiring_cache_try_get_or_set_with_err_keeps_expired() {
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::new();
        c.set(1, ExpiredU8(15)); // expired
        let result: Result<&mut ExpiredU8, &str> = c.cache_try_get_or_set_with(1, || Err("fail"));
        assert!(result.is_err());
        assert_eq!(c.cache_size(), 1, "expired entry must remain after Err");
        assert_eq!(c.cache_evictions(), Some(0));
        // miss is counted before f() is called, so it's Some(1) even on Err
        assert_eq!(c.cache_misses(), Some(1));
    }

    #[test]
    fn expiring_cache_try_get_or_set_with_ok_evicts_expired() {
        use std::sync::{Arc, Mutex};
        let fired: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(vec![]));
        let fired2 = fired.clone();
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder()
            .on_evict(move |k: &u8, _v: &ExpiredU8| {
                fired2.lock().unwrap().push(*k);
            })
            .build();
        c.set(1, ExpiredU8(15)); // expired
        let result: Result<&mut ExpiredU8, &str> =
            c.cache_try_get_or_set_with(1, || Ok(ExpiredU8(3)));
        assert_eq!(*result.unwrap(), ExpiredU8(3));
        assert_eq!(c.cache_evictions(), Some(1));
        assert_eq!(c.cache_misses(), Some(1));
        assert_eq!(fired.lock().unwrap().clone(), vec![1]);
    }

    #[test]
    fn expiring_cache_clear_no_on_evict() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering as AOrdering},
            Arc,
        };
        let count = Arc::new(AtomicUsize::new(0));
        let count2 = count.clone();
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder()
            .on_evict(move |_k: &u8, _v: &ExpiredU8| {
                count2.fetch_add(1, AOrdering::Relaxed);
            })
            .build();
        c.set(1, ExpiredU8(5));
        c.set(2, ExpiredU8(15));
        c.cache_clear();
        assert_eq!(c.cache_size(), 0);
        assert_eq!(
            count.load(AOrdering::Relaxed),
            0,
            "on_evict must not fire on cache_clear"
        );
    }

    #[test]
    fn expiring_cache_reset_clears_metrics_and_entries() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering as AOrdering},
            Arc,
        };
        let count = Arc::new(AtomicUsize::new(0));
        let count2 = count.clone();
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder()
            .on_evict(move |_k: &u8, _v: &ExpiredU8| {
                count2.fetch_add(1, AOrdering::Relaxed);
            })
            .build();
        c.set(1, ExpiredU8(5));
        c.get(&1); // 1 hit
        c.cache_reset();
        assert_eq!(c.cache_size(), 0);
        assert_eq!(c.cache_hits(), Some(0));
        assert_eq!(c.cache_misses(), Some(0));
        assert_eq!(c.cache_evictions(), Some(0));
        assert_eq!(
            count.load(AOrdering::Relaxed),
            0,
            "on_evict must not fire on cache_reset"
        );
    }

    #[test]
    fn expiring_cache_peek_expired_no_metrics_no_removal() {
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::new();
        c.set(1, ExpiredU8(15)); // expired
        assert!(c.cache_peek(&1).is_none());
        // metrics unchanged
        assert_eq!(c.cache_hits(), Some(0));
        assert_eq!(c.cache_misses(), Some(0));
        assert_eq!(c.cache_evictions(), Some(0));
        // entry still present (peek does not remove)
        assert_eq!(c.cache_size(), 1);
    }

    #[test]
    fn expiring_cache_peek_live_no_metrics_change() {
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::new();
        c.set(1, ExpiredU8(5));
        assert_eq!(c.cache_peek(&1), Some(&ExpiredU8(5)));
        assert_eq!(c.cache_hits(), Some(0));
        assert_eq!(c.cache_misses(), Some(0));
    }

    #[test]
    fn expiring_cache_iter_excludes_expired() {
        use crate::CachedIter;
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::new();
        c.set(1, ExpiredU8(5)); // live
        c.set(2, ExpiredU8(15)); // expired
        c.set(3, ExpiredU8(3)); // live
        let mut live: Vec<u8> = CachedIter::iter(&c).map(|(k, _)| *k).collect();
        live.sort();
        assert_eq!(live, vec![1, 3]);
    }

    #[test]
    fn expiring_cache_get_with_expiry_status_hit() {
        use crate::CloneCached;
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::new();
        c.set(1, ExpiredU8(5));
        let (val, expired) = c.cache_get_with_expiry_status(&1);
        assert_eq!(val, Some(ExpiredU8(5)));
        assert!(!expired);
        assert_eq!(c.cache_hits(), Some(1));
    }

    #[test]
    fn expiring_cache_get_with_expiry_status_expired() {
        use crate::CloneCached;
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::new();
        c.set(1, ExpiredU8(15));
        let (val, expired) = c.cache_get_with_expiry_status(&1);
        assert_eq!(val, Some(ExpiredU8(15)));
        assert!(expired);
        assert_eq!(c.cache_misses(), Some(1));
    }

    #[test]
    fn expiring_cache_get_with_expiry_status_miss() {
        use crate::CloneCached;
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::new();
        let (val, expired) = c.cache_get_with_expiry_status(&99u8);
        assert_eq!(val, None);
        assert!(!expired);
        assert_eq!(c.cache_misses(), Some(1));
    }

    #[test]
    fn expiring_cache_debug_format() {
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::new();
        c.set(1, ExpiredU8(5));
        c.get(&1); // 1 hit
        let s = format!("{:?}", c);
        assert!(s.contains("ExpiringCache"), "missing struct name in Debug");
        assert!(s.contains("hits"), "missing hits field in Debug");
        assert!(s.contains("misses"), "missing misses field in Debug");
        assert!(s.contains("evictions"), "missing evictions field in Debug");
    }

    #[test]
    fn expiring_cache_clone_independent() {
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::new();
        c.set(1, ExpiredU8(5));
        c.get(&1); // 1 hit
        let mut c2 = c.clone();
        assert_eq!(c2.cache_hits(), Some(1));
        assert_eq!(c2.cache_size(), 1);
        // mutations to c2 don't affect c
        c2.get(&1);
        assert_eq!(c.cache_hits(), Some(1));
        assert_eq!(c2.cache_hits(), Some(2));
    }

    #[test]
    fn expiring_cache_try_build() {
        let result: Result<ExpiringCache<u8, ExpiredU8>, _> =
            ExpiringCache::builder().capacity(10).try_build();
        assert!(result.is_ok());
        let c = result.unwrap();
        assert_eq!(c.cache_size(), 0);
    }
}
