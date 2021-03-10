use super::{Cached, SizedCache};
use std::cmp::Eq;
use std::hash::Hash;
use std::time::Instant;

#[cfg(feature = "async")]
use {super::CachedAsync, async_trait::async_trait, futures_lite::Future};

/// Timed LRU Cache
///
/// Stores a limited number of values,
/// evicting expired and least-used entries.
/// Time expiration is determined based on entry insertion time..
/// The TTL of an entry is not updated when retrieved.
///
/// Note: This cache is in-memory only
#[derive(Clone, Debug)]
pub struct TimedSizedCache<K, V> {
    pub(super) store: SizedCache<K, (Instant, V)>,
    pub(super) size: usize,
    pub(super) seconds: u64,
    pub(super) hits: u64,
    pub(super) misses: u64,
}

impl<K: Hash + Eq + Clone, V> TimedSizedCache<K, V> {
    /// Creates a new `SizedCache` with a given size limit and pre-allocated backing data
    pub fn with_size_and_lifespan(size: usize, seconds: u64) -> TimedSizedCache<K, V> {
        if size == 0 {
            panic!("`size` of `TimedSizedCache` must be greater than zero.")
        }
        TimedSizedCache {
            store: SizedCache::with_size(size),
            size,
            seconds,
            hits: 0,
            misses: 0,
        }
    }

    fn iter_order(&self) -> impl Iterator<Item = &(K, (Instant, V))> {
        let max_seconds = self.seconds;
        self.store
            .iter_order()
            .filter(move |(_k, stamped)| stamped.0.elapsed().as_secs() < max_seconds)
    }

    /// Return an iterator of keys in the current order from most
    /// to least recently used.
    /// Items passed their expiration seconds will be excluded.
    pub fn key_order(&self) -> impl Iterator<Item = &K> {
        self.iter_order().map(|(k, _v)| k)
    }

    /// Return an iterator of timestamped values in the current order
    /// from most to least recently used.
    /// Items passed their expiration seconds will be excluded.
    pub fn value_order(&self) -> impl Iterator<Item = &(Instant, V)> {
        self.iter_order().map(|(_k, v)| v)
    }
}

impl<K: Hash + Eq + Clone, V> Cached<K, V> for TimedSizedCache<K, V> {
    fn cache_get(&mut self, key: &K) -> Option<&V> {
        let max_seconds = self.seconds;
        let val = self
            .store
            .get_if(key, |stamped| stamped.0.elapsed().as_secs() < max_seconds);
        match val {
            None => {
                self.misses += 1;
                None
            }
            Some(stamped) => {
                self.hits += 1;
                Some(&stamped.1)
            }
        }
    }

    fn cache_get_mut(&mut self, key: &K) -> std::option::Option<&mut V> {
        let max_seconds = self.seconds;
        let val = self
            .store
            .get_mut_if(key, |stamped| stamped.0.elapsed().as_secs() < max_seconds);
        match val {
            None => {
                self.misses += 1;
                None
            }
            Some(stamped) => {
                self.hits += 1;
                Some(&mut stamped.1)
            }
        }
    }

    fn cache_get_or_set_with<F: FnOnce() -> V>(&mut self, key: K, f: F) -> &mut V {
        let setter = || (Instant::now(), f());
        let max_seconds = self.seconds;
        let (was_present, was_valid, stamped) =
            self.store.get_or_set_with_if(key, setter, |stamped| {
                stamped.0.elapsed().as_secs() < max_seconds
            });
        if was_present && was_valid {
            self.hits += 1;
        } else {
            self.misses += 1;
        }
        &mut stamped.1
    }

    fn cache_set(&mut self, key: K, val: V) -> Option<V> {
        let stamped = self.store.cache_set(key, (Instant::now(), val));
        stamped.map(|stamped| stamped.1)
    }

    fn cache_remove(&mut self, k: &K) -> Option<V> {
        let stamped = self.store.cache_remove(k);
        stamped.map(|stamped| stamped.1)
    }
    fn cache_clear(&mut self) {
        self.store.cache_clear();
    }
    fn cache_reset(&mut self) {
        self.cache_clear();
    }
    fn cache_size(&self) -> usize {
        self.store.cache_size()
    }
    fn cache_hits(&self) -> Option<u64> {
        Some(self.hits)
    }
    fn cache_misses(&self) -> Option<u64> {
        Some(self.misses)
    }
    fn cache_capacity(&self) -> Option<usize> {
        Some(self.size)
    }
    fn cache_lifespan(&self) -> Option<u64> {
        Some(self.seconds)
    }
    fn cache_set_lifespan(&mut self, seconds: u64) -> Option<u64> {
        let old = self.seconds;
        self.seconds = seconds;
        Some(old)
    }
}

#[cfg(feature = "async")]
#[async_trait]
impl<K, V> CachedAsync<K, V> for TimedSizedCache<K, V>
where
    K: Hash + Eq + Clone + Send,
{
    async fn get_or_set_with<F, Fut>(&mut self, key: K, f: F) -> &mut V
    where
        V: Send,
        F: FnOnce() -> Fut + Send,
        Fut: Future<Output = V> + Send,
    {
        let setter = || async { (Instant::now(), f().await) };
        let max_seconds = self.seconds;
        let (was_present, was_valid, stamped) = self
            .store
            .get_or_set_with_if_async(key, setter, |stamped| {
                stamped.0.elapsed().as_secs() < max_seconds
            })
            .await;
        if was_present && was_valid {
            self.hits += 1;
        } else {
            self.misses += 1;
        }
        &mut stamped.1
    }

    async fn try_get_or_set_with<F, Fut, E>(&mut self, key: K, f: F) -> Result<&mut V, E>
    where
        V: Send,
        F: FnOnce() -> Fut + Send,
        Fut: Future<Output = Result<V, E>> + Send,
    {
        let setter = || async {
            let new_val = f().await?;
            Ok((Instant::now(), new_val))
        };
        let max_seconds = self.seconds;
        let (was_present, was_valid, stamped) = self
            .store
            .try_get_or_set_with_if_async(key, setter, |stamped| {
                stamped.0.elapsed().as_secs() < max_seconds
            })
            .await?;
        if was_present && was_valid {
            self.hits += 1;
        } else {
            self.misses += 1;
        }
        Ok(&mut stamped.1)
    }
}

#[cfg(test)]
/// Cache store tests
mod tests {
    use std::{thread::sleep, time::Duration};

    use super::*;

    #[test]
    fn timed_sized_cache() {
        let mut c = TimedSizedCache::with_size_and_lifespan(5, 2);
        assert!(c.cache_get(&1).is_none());
        let misses = c.cache_misses().unwrap();
        assert_eq!(1, misses);

        assert_eq!(c.cache_set(1, 100), None);
        assert!(c.cache_get(&1).is_some());
        let hits = c.cache_hits().unwrap();
        let misses = c.cache_misses().unwrap();
        assert_eq!(1, hits);
        assert_eq!(1, misses);

        assert_eq!(c.cache_set(2, 100), None);
        assert_eq!(c.cache_set(3, 100), None);
        assert_eq!(c.cache_set(4, 100), None);
        assert_eq!(c.cache_set(5, 100), None);

        assert_eq!(c.key_order().cloned().collect::<Vec<_>>(), [5, 4, 3, 2, 1]);

        sleep(Duration::new(1, 0));

        assert_eq!(c.cache_set(6, 100), None);
        assert_eq!(c.cache_set(7, 100), None);

        assert_eq!(c.key_order().cloned().collect::<Vec<_>>(), [7, 6, 5, 4, 3]);

        assert!(c.cache_get(&2).is_none());
        assert!(c.cache_get(&3).is_some());

        assert_eq!(c.key_order().cloned().collect::<Vec<_>>(), [3, 7, 6, 5, 4]);

        assert_eq!(2, c.cache_misses().unwrap());
        assert_eq!(5, c.cache_size());

        sleep(Duration::new(1, 0));

        assert!(c.cache_get(&1).is_none());
        assert!(c.cache_get(&2).is_none());
        assert!(c.cache_get(&3).is_none());
        assert!(c.cache_get(&4).is_none());
        assert!(c.cache_get(&5).is_none());
        assert!(c.cache_get(&6).is_some());
        assert!(c.cache_get(&7).is_some());

        assert_eq!(7, c.cache_misses().unwrap());

        assert!(c.cache_set(1, 100).is_none());
        assert!(c.cache_set(2, 100).is_none());
        assert!(c.cache_set(3, 100).is_none());
        assert_eq!(c.key_order().cloned().collect::<Vec<_>>(), [3, 2, 1, 7, 6]);

        sleep(Duration::new(1, 0));

        assert!(c.cache_get(&1).is_some());
        assert!(c.cache_get(&2).is_some());
        assert!(c.cache_get(&3).is_some());
        assert!(c.cache_get(&4).is_none());
        assert!(c.cache_get(&5).is_none());
        assert!(c.cache_get(&6).is_none());
        assert!(c.cache_get(&7).is_none());

        assert_eq!(11, c.cache_misses().unwrap());

        let mut c = TimedSizedCache::with_size_and_lifespan(5, 0);
        let mut ticker = 0;
        let setter = || {
            let v = ticker;
            ticker += 1;
            v
        };
        assert_eq!(c.cache_get_or_set_with(1, setter), &0);
        let setter = || {
            let v = ticker;
            ticker += 1;
            v
        };
        assert_eq!(c.cache_get_or_set_with(1, setter), &1);
    }

    #[test]
    fn clear() {
        let mut c = TimedSizedCache::with_size_and_lifespan(3, 3600);

        assert_eq!(c.cache_set(1, 100), None);
        assert_eq!(c.cache_set(2, 200), None);
        assert_eq!(c.cache_set(3, 300), None);
        c.cache_clear();

        assert_eq!(0, c.cache_size());
    }

    #[test]
    fn reset() {
        let init_capacity = 1;
        let mut c = TimedSizedCache::with_size_and_lifespan(init_capacity, 100);
        assert_eq!(c.cache_set(1, 100), None);
        assert_eq!(c.cache_set(2, 200), None);
        assert_eq!(c.cache_set(3, 300), None);
        assert!(init_capacity <= c.store.capacity);

        c.cache_reset();
        assert!(init_capacity <= c.store.capacity);
    }

    #[test]
    fn remove() {
        let mut c = TimedSizedCache::with_size_and_lifespan(3, 3600);

        assert_eq!(c.cache_set(1, 100), None);
        assert_eq!(c.cache_set(2, 200), None);
        assert_eq!(c.cache_set(3, 300), None);

        assert_eq!(Some(100), c.cache_remove(&1));
        assert_eq!(2, c.cache_size());

        assert_eq!(Some(200), c.cache_remove(&2));
        assert_eq!(1, c.cache_size());

        assert_eq!(None, c.cache_remove(&2));
        assert_eq!(1, c.cache_size());

        assert_eq!(Some(300), c.cache_remove(&3));
        assert_eq!(0, c.cache_size());
    }

    #[test]
    fn get_or_set_with() {
        let mut c = TimedSizedCache::with_size_and_lifespan(5, 2);

        assert_eq!(c.cache_get_or_set_with(0, || 0), &0);
        assert_eq!(c.cache_get_or_set_with(1, || 1), &1);
        assert_eq!(c.cache_get_or_set_with(2, || 2), &2);
        assert_eq!(c.cache_get_or_set_with(3, || 3), &3);
        assert_eq!(c.cache_get_or_set_with(4, || 4), &4);
        assert_eq!(c.cache_get_or_set_with(5, || 5), &5);

        assert_eq!(c.cache_misses(), Some(6));

        assert_eq!(c.cache_get_or_set_with(0, || 0), &0);

        assert_eq!(c.cache_misses(), Some(7));

        assert_eq!(c.cache_get_or_set_with(0, || 42), &0);

        sleep(Duration::new(1, 0));

        assert_eq!(c.cache_get_or_set_with(0, || 42), &0);

        assert_eq!(c.cache_get_or_set_with(1, || 1), &1);

        assert_eq!(c.cache_get_or_set_with(4, || 42), &4);

        assert_eq!(c.cache_get_or_set_with(5, || 42), &5);

        assert_eq!(c.cache_get_or_set_with(6, || 6), &6);

        assert_eq!(c.cache_misses(), Some(9));

        sleep(Duration::new(1, 0));

        assert_eq!(c.cache_get_or_set_with(4, || 42), &42);

        assert_eq!(c.cache_get_or_set_with(5, || 42), &42);

        assert_eq!(c.cache_get_or_set_with(6, || 42), &6);

        assert_eq!(c.cache_misses(), Some(11));
    }

    #[cfg(feature = "async")]
    #[tokio::test]
    async fn test_async_trait_timed_sized() {
        use crate::CachedAsync;
        let mut c = TimedSizedCache::with_size_and_lifespan(5, 1);

        async fn _get(n: usize) -> usize {
            n
        }

        assert_eq!(c.get_or_set_with(0, || async { _get(0).await }).await, &0);
        assert_eq!(c.get_or_set_with(1, || async { _get(1).await }).await, &1);
        assert_eq!(c.get_or_set_with(2, || async { _get(2).await }).await, &2);
        assert_eq!(c.get_or_set_with(3, || async { _get(3).await }).await, &3);

        assert_eq!(c.get_or_set_with(0, || async { _get(3).await }).await, &0);
        assert_eq!(c.get_or_set_with(1, || async { _get(3).await }).await, &1);
        assert_eq!(c.get_or_set_with(2, || async { _get(3).await }).await, &2);
        assert_eq!(c.get_or_set_with(3, || async { _get(1).await }).await, &3);

        sleep(Duration::new(1, 0));
        // after sleeping, the original val should have expired
        assert_eq!(c.get_or_set_with(0, || async { _get(3).await }).await, &3);

        c.cache_reset();
        async fn _try_get(n: usize) -> Result<usize, String> {
            if n < 10 {
                Ok(n)
            } else {
                Err("dead".to_string())
            }
        }

        assert_eq!(
            c.try_get_or_set_with(0, || async {
                match _try_get(0).await {
                    Ok(n) => Ok(n),
                    Err(_) => Err("err".to_string()),
                }
            })
            .await
            .unwrap(),
            &0
        );
        assert_eq!(
            c.try_get_or_set_with(0, || async {
                match _try_get(5).await {
                    Ok(n) => Ok(n),
                    Err(_) => Err("err".to_string()),
                }
            })
            .await
            .unwrap(),
            &0
        );

        c.cache_reset();
        let res: Result<&mut usize, String> = c
            .try_get_or_set_with(0, || async { Ok(_try_get(10).await?) })
            .await;
        assert!(res.is_err());
        assert!(c.key_order().next().is_none());

        let res: Result<&mut usize, String> = c
            .try_get_or_set_with(0, || async { Ok(_try_get(1).await?) })
            .await;
        assert_eq!(res.unwrap(), &1);
        let res: Result<&mut usize, String> = c
            .try_get_or_set_with(0, || async { Ok(_try_get(5).await?) })
            .await;
        assert_eq!(res.unwrap(), &1);
        sleep(Duration::new(1, 0));
        let res: Result<&mut usize, String> = c
            .try_get_or_set_with(0, || async { Ok(_try_get(5).await?) })
            .await;
        assert_eq!(res.unwrap(), &5);
    }
}
