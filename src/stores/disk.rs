use crate::IOCached;
use directories::BaseDirs;
use instant::Duration;
use serde::de::DeserializeOwned;
use serde::Serialize;
use sled::Db;
use std::{fmt::Display, path::PathBuf, time::SystemTime};
use std::marker::PhantomData;

pub struct DiskCacheBuilder<K, V> {
    seconds: u64,
    refresh: bool,
    namespace: String,
    prefix: String,
    disk_path: Option<PathBuf>,
    _phantom_k: PhantomData<K>,
    _phantom_v: PhantomData<V>,
}

const ENV_KEY: &str = "CACHED_DISK_PATH";
const DEFAULT_NAMESPACE: &str = "cached-disk-store:";
const LAST_CLEANUP_KEY: &str = "last-cleanup";

use thiserror::Error;

#[derive(Error, Debug)]
pub enum DiskCacheBuildError {
    #[error("Storage connection error")]
    ConnectionError(#[from] sled::Error),
    #[error("Connection string not specified or invalid in env var {env_key:?}: {error:?}")]
    MissingDiskPath {
        env_key: String,
        error: std::env::VarError,
    },
}

impl<K, V> DiskCacheBuilder<K, V>
where
    K: Display,
    V: Serialize + DeserializeOwned,
{
    /// Initialize a `DiskCacheBuilder`
    pub fn new<S: AsRef<str>>(prefix: S, seconds: u64) -> DiskCacheBuilder<K, V> {
        Self {
            seconds,
            refresh: false,
            namespace: DEFAULT_NAMESPACE.to_string(),
            prefix: prefix.as_ref().to_string(),
            disk_path: None,
            _phantom_k: Default::default(),
            _phantom_v: Default::default(),
        }
    }

    /// Specify the cache TTL/lifespan in seconds
    pub fn set_lifespan(mut self, seconds: u64) -> Self {
        self.seconds = seconds;
        self
    }

    /// Specify whether cache hits refresh the TTL
    pub fn set_refresh(mut self, refresh: bool) -> Self {
        self.refresh = refresh;
        self
    }

    /// Set the namespace for cache keys. Defaults to `cached-disk-store:`.
    /// Used to generate keys formatted as: `{namespace}{prefix}{key}`
    /// Note that no delimiters are implicitly added so you may pass
    /// an empty string if you want there to be no namespace on keys.
    pub fn set_namespace<S: AsRef<str>>(mut self, namespace: S) -> Self {
        self.namespace = namespace.as_ref().to_string();
        self
    }

    /// Set the prefix for cache keys.
    /// Used to generate keys formatted as: `{namespace}{prefix}{key}`
    /// Note that no delimiters are implicitly added so you may pass
    /// an empty string if you want there to be no prefix on keys.
    pub fn set_prefix<S: AsRef<str>>(mut self, prefix: S) -> Self {
        self.prefix = prefix.as_ref().to_string();
        self
    }

    /// Set the disk path for where the data will be stored
    pub fn set_disk_path(mut self, cs: PathBuf) -> Self {
        self.disk_path = Some(cs);
        self
    }

    /// Return the disk path, or load from the env var: CACHED_DISK_PATH, or fall back to OS cache directory
    pub fn disk_path(&self) -> Result<PathBuf, DiskCacheBuildError> {
        match self.disk_path {
            Some(ref s) => Ok(s.to_path_buf()),
            None => match std::env::var(ENV_KEY) {
                Ok(path) => Ok(PathBuf::from(path)),
                Err(error) => {
                    let disk_path = BaseDirs::new().map(|base_dirs|
                        base_dirs
                            .cache_dir()
                            .join(env!("CARGO_PKG_NAME"))
                            .join("cached")
                    );

                    match disk_path {
                        Some(path) => Ok(path),
                        None => Err(DiskCacheBuildError::MissingDiskPath {
                            env_key: ENV_KEY.to_string(),
                            error,
                        })
                    }
                }
            }
        }
    }

    pub fn build(self) -> Result<DiskCache<K, V>, DiskCacheBuildError> {
        let disk_path = self.disk_path()?;
        let connection = sled::open(disk_path.clone())?;

        Ok(DiskCache {
            connection,
            seconds: self.seconds,
            refresh: self.refresh,
            disk_path,
            namespace: self.namespace,
            prefix: self.prefix,
            _phantom_k: self._phantom_k,
            _phantom_v: self._phantom_v,
        })
    }
}

/// Cache store backed by disk
pub struct DiskCache<K, V> {
    pub(super) seconds: u64,
    pub(super) refresh: bool,
    pub(super) namespace: String,
    pub(super) prefix: String,
    connection: sled::Db,
    disk_path: PathBuf,
    _phantom_k: PhantomData<K>,
    _phantom_v: PhantomData<V>,
}

impl<K, V> DiskCache<K, V>
where
    K: Display,
    V: Serialize + DeserializeOwned,
{
    #[allow(clippy::new_ret_no_self)]
    /// Initialize a `DiskCacheBuilder`
    pub fn new<S: AsRef<str>>(prefix: S, seconds: u64) -> DiskCacheBuilder<K, V> {
        DiskCacheBuilder::new(prefix, seconds)
    }

    fn generate_key(&self, key: &K) -> String {
        format!("{}{}{}", self.namespace, self.prefix, key)
    }

    /// Return the disk path used
    pub fn disk_path(&self) -> PathBuf {
        self.disk_path.clone()
    }

    fn last_cleanup(connection: &Db) -> SystemTime {
        match connection.get(LAST_CLEANUP_KEY) {
            Ok(Some(l)) => match rmp_serde::from_slice::<SystemTime>(&l) {
                Ok(l) => l,
                _ => SystemTime::UNIX_EPOCH
            },
            _ => SystemTime::UNIX_EPOCH
        }
    }

    fn cleanup_expired_entries(connection: &Db) {
        let now = SystemTime::now();
        let last_cleanup = DiskCache::<K, V>::last_cleanup(connection);

        if last_cleanup + Duration::from_secs(10) < now {
            return;
        }

        for (key, value) in connection.iter().flatten() {
            if let Ok(cached) = rmp_serde::from_slice::<CachedDiskValue<V>>(&value) {
                if let Some(expires) = cached.expires {
                    if now > expires {
                        let _ = connection.remove(key);
                    }
                }
            }
        }

        let _ = connection.insert(LAST_CLEANUP_KEY, rmp_serde::to_vec(&SystemTime::now()).unwrap());
    }
}

#[derive(Error, Debug)]
pub enum DiskCacheError {
    #[error("Storage error")]
    StorageError(#[from] sled::Error),
    #[error("Error deserializing cached value")]
    CacheDeserializtionError(#[from] rmp_serde::decode::Error),
    #[error("Error serializing cached value")]
    CacheSerializtionError(#[from] rmp_serde::encode::Error),
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CachedDiskValue<V> {
    pub(crate) value: V,
    pub(crate) expires: Option<SystemTime>,
    pub(crate) version: Option<u64>,
}

impl<V> CachedDiskValue<V> {
    fn new(value: V, expires: Option<SystemTime>) -> Self {
        Self {
            value,
            expires,
            version: Some(1),
        }
    }
}

impl<K, V> IOCached<K, V> for DiskCache<K, V>
where
    K: Display,
    V: Serialize + DeserializeOwned,
{
    type Error = DiskCacheError;

    fn cache_get(&self, key: &K) -> Result<Option<V>, DiskCacheError> {
        Self::cleanup_expired_entries(&self.connection);

        let key = self.generate_key(key);

        if let Some(data) = self.connection.get(key)? {
            let cached = rmp_serde::from_slice::<CachedDiskValue<V>>(&data)?;

            if let Some(expires) = cached.expires {
                if SystemTime::now() < expires {
                    Ok(Some(cached.value))
                } else {
                    Ok(None)
                }
            } else {
                Ok(Some(cached.value))
            }
        } else {
            Ok(None)
        }
    }

    fn cache_set(&self, key: K, value: V) -> Result<Option<V>, DiskCacheError> {
        // TODO: wrap value when serializing to also add expiration data, then add separate thread to clean up the cache?
        let key = self.generate_key(&key);
        let value = rmp_serde::to_vec(&CachedDiskValue::new(
            value,
            Some(SystemTime::now() + Duration::from_secs(self.seconds)),
        ))?;

        if let Some(data) = self.connection.insert(key, value)? {
            let cached = rmp_serde::from_slice::<CachedDiskValue<V>>(&data)?;

            if let Some(expires) = cached.expires {
                if SystemTime::now() < expires {
                    Ok(Some(cached.value))
                } else {
                    Ok(None)
                }
            } else {
                Ok(Some(cached.value))
            }
        } else {
            Ok(None)
        }
    }

    fn cache_remove(&self, key: &K) -> Result<Option<V>, DiskCacheError> {
        let key = self.generate_key(key);

        if let Some(data) = self.connection.remove(key)? {
            let cached = rmp_serde::from_slice::<CachedDiskValue<V>>(&data)?;

            if let Some(expires) = cached.expires {
                if SystemTime::now() < expires {
                    Ok(Some(cached.value))
                } else {
                    Ok(None)
                }
            } else {
                Ok(Some(cached.value))
            }
        } else {
            Ok(None)
        }
    }

    fn cache_lifespan(&self) -> Option<u64> {
        Some(self.seconds)
    }

    fn cache_set_lifespan(&mut self, seconds: u64) -> Option<u64> {
        let old = self.seconds;
        self.seconds = seconds;
        Some(old)
    }

    fn cache_set_refresh(&mut self, refresh: bool) -> bool {
        let old = self.refresh;
        self.refresh = refresh;
        old
    }
}

/*
#[cfg(all(
    feature = "async",
    any(feature = "disk_async_std", feature = "disk_tokio")
))]
mod async_disk {
    use super::*;
    use {crate::IOCachedAsync, async_trait::async_trait};

    pub struct AsyncDiskCacheBuilder<K, V> {
        seconds: u64,
        refresh: bool,
        namespace: String,
        prefix: String,
        connection_string: Option<String>,
        _phantom_k: PhantomData<K>,
        _phantom_v: PhantomData<V>,
    }

    impl<K, V> AsyncDiskCacheBuilder<K, V>
    where
        K: Display,
        V: Serialize + DeserializeOwned,
    {
        /// Initialize a `DiskCacheBuilder`
        pub fn new<S: AsRef<str>>(prefix: S, seconds: u64) -> AsyncDiskCacheBuilder<K, V> {
            Self {
                seconds,
                refresh: false,
                namespace: DEFAULT_NAMESPACE.to_string(),
                prefix: prefix.as_ref().to_string(),
                connection_string: None,
                _phantom_k: Default::default(),
                _phantom_v: Default::default(),
            }
        }

        /// Specify the cache TTL/lifespan in seconds
        pub fn set_lifespan(mut self, seconds: u64) -> Self {
            self.seconds = seconds;
            self
        }

        /// Specify whether cache hits refresh the TTL
        pub fn set_refresh(mut self, refresh: bool) -> Self {
            self.refresh = refresh;
            self
        }

        /// Set the namespace for cache keys. Defaults to `cached-disk-store:`.
        /// Used to generate keys formatted as: `{namespace}{prefix}{key}`
        /// Note that no delimiters are implicitly added so you may pass
        /// an empty string if you want there to be no namespace on keys.
        pub fn set_namespace<S: AsRef<str>>(mut self, namespace: S) -> Self {
            self.namespace = namespace.as_ref().to_string();
            self
        }

        /// Set the prefix for cache keys
        /// Used to generate keys formatted as: `{namespace}{prefix}{key}`
        /// Note that no delimiters are implicitly added so you may pass
        /// an empty string if you want there to be no prefix on keys.
        pub fn set_prefix<S: AsRef<str>>(mut self, prefix: S) -> Self {
            self.prefix = prefix.as_ref().to_string();
            self
        }

        /// Set the connection string for disk
        pub fn set_connection_string(mut self, cs: &str) -> Self {
            self.connection_string = Some(cs.to_string());
            self
        }

        /// Return the current connection string or load from the env var: CACHED_DISK_PATH
        pub fn connection_string(&self) -> Result<String, DiskCacheBuildError> {
            match self.connection_string {
                Some(ref s) => Ok(s.to_string()),
                None => std::env::var(ENV_KEY).map_err(|e| {
                    DiskCacheBuildError::MissingDiskPath {
                        env_key: ENV_KEY.to_string(),
                        error: e,
                    }
                }),
            }
        }

        async fn create_multiplexed_connection(
            &self,
        ) -> Result<disk::aio::MultiplexedConnection, DiskCacheBuildError> {
            let s = self.connection_string()?;
            let client = disk::Client::open(s)?;
            let conn = client.get_multiplexed_async_connection().await?;
            Ok(conn)
        }

        pub async fn build(self) -> Result<AsyncDiskCache<K, V>, DiskCacheBuildError> {
            Ok(AsyncDiskCache {
                seconds: self.seconds,
                refresh: self.refresh,
                connection_string: self.connection_string()?,
                multiplexed_connection: self.create_multiplexed_connection().await?,
                namespace: self.namespace,
                prefix: self.prefix,
                _phantom_k: self._phantom_k,
                _phantom_v: self._phantom_v,
            })
        }
    }

    /// Cache store backed by disk
    ///
    /// Values have a ttl applied and enforced by disk.
    /// Uses a `disk::aio::MultiplexedConnection` under the hood.
    pub struct AsyncDiskCache<K, V> {
        pub(super) seconds: u64,
        pub(super) refresh: bool,
        pub(super) namespace: String,
        pub(super) prefix: String,
        connection_string: String,
        multiplexed_connection: disk::aio::MultiplexedConnection,
        _phantom_k: PhantomData<K>,
        _phantom_v: PhantomData<V>,
    }

    impl<K, V> AsyncDiskCache<K, V>
    where
        K: Display + Send + Sync,
        V: Serialize + DeserializeOwned + Send + Sync,
    {
        #[allow(clippy::new_ret_no_self)]
        /// Initialize an `AsyncDiskCacheBuilder`
        pub fn new<S: AsRef<str>>(prefix: S, seconds: u64) -> AsyncDiskCacheBuilder<K, V> {
            AsyncDiskCacheBuilder::new(prefix, seconds)
        }

        fn generate_key(&self, key: &K) -> String {
            format!("{}{}{}", self.namespace, self.prefix, key)
        }

        /// Return the disk connection string used
        pub fn connection_string(&self) -> String {
            self.connection_string.clone()
        }
    }

    #[async_trait]
    impl<'de, K, V> IOCachedAsync<K, V> for AsyncDiskCache<K, V>
    where
        K: Display + Send + Sync,
        V: Serialize + DeserializeOwned + Send + Sync,
    {
        type Error = DiskCacheError;

        /// Get a cached value
        async fn cache_get(&self, key: &K) -> Result<Option<V>, Self::Error> {
            let mut conn = self.multiplexed_connection.clone();
            let mut pipe = disk::pipe();
            let key = self.generate_key(key);

            pipe.get(key.clone());
            if self.refresh {
                pipe.expire(key, self.seconds as usize).ignore();
            }
            let res: (Option<String>,) = pipe.query_async(&mut conn).await?;
            match res.0 {
                None => Ok(None),
                Some(s) => {
                    let v: CachedDiskValue<V> = serde_json::from_str(&s).map_err(|e| {
                        DiskCacheError::CacheDeserializationError {
                            cached_value: s,
                            error: e,
                        }
                    })?;
                    Ok(Some(v.value))
                }
            }
        }

        /// Set a cached value
        async fn cache_set(&self, key: K, val: V) -> Result<Option<V>, Self::Error> {
            let mut conn = self.multiplexed_connection.clone();
            let mut pipe = disk::pipe();
            let key = self.generate_key(&key);

            let val = CachedDiskValue::new(val);
            pipe.get(key.clone());
            pipe.set_ex::<String, String>(
                key,
                serde_json::to_string(&val)
                    .map_err(|e| DiskCacheError::CacheSerializationError { error: e })?,
                self.seconds as usize,
            )
            .ignore();

            let res: (Option<String>,) = pipe.query_async(&mut conn).await?;
            match res.0 {
                None => Ok(None),
                Some(s) => {
                    let v: CachedDiskValue<V> = serde_json::from_str(&s).map_err(|e| {
                        DiskCacheError::CacheDeserializationError {
                            cached_value: s,
                            error: e,
                        }
                    })?;
                    Ok(Some(v.value))
                }
            }
        }

        /// Remove a cached value
        async fn cache_remove(&self, key: &K) -> Result<Option<V>, Self::Error> {
            let mut conn = self.multiplexed_connection.clone();
            let mut pipe = disk::pipe();
            let key = self.generate_key(key);

            pipe.get(key.clone());
            pipe.del::<String>(key).ignore();
            let res: (Option<String>,) = pipe.query_async(&mut conn).await?;
            match res.0 {
                None => Ok(None),
                Some(s) => {
                    let v: CachedDiskValue<V> = serde_json::from_str(&s).map_err(|e| {
                        DiskCacheError::CacheDeserializationError {
                            cached_value: s,
                            error: e,
                        }
                    })?;
                    Ok(Some(v.value))
                }
            }
        }

        /// Set the flag to control whether cache hits refresh the ttl of cached values, returns the old flag value
        fn cache_set_refresh(&mut self, refresh: bool) -> bool {
            let old = self.refresh;
            self.refresh = refresh;
            old
        }

        /// Return the lifespan of cached values (time to eviction)
        fn cache_lifespan(&self) -> Option<u64> {
            Some(self.seconds)
        }

        /// Set the lifespan of cached values, returns the old value
        fn cache_set_lifespan(&mut self, seconds: u64) -> Option<u64> {
            let old = self.seconds;
            self.seconds = seconds;
            Some(old)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::thread::sleep;
        use std::time::Duration;

        fn now_millis() -> u128 {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        }

        #[tokio::test]
        async fn test_async_disk_cache() {
            let mut c: AsyncDiskCache<u32, u32> =
                AsyncDiskCache::new(format!("{}:async-disk-cache-test", now_millis()), 2)
                    .build()
                    .await
                    .unwrap();

            assert!(c.cache_get(&1).await.unwrap().is_none());

            assert!(c.cache_set(1, 100).await.unwrap().is_none());
            assert!(c.cache_get(&1).await.unwrap().is_some());

            sleep(Duration::new(2, 500000));
            assert!(c.cache_get(&1).await.unwrap().is_none());

            let old = c.cache_set_lifespan(1).unwrap();
            assert_eq!(2, old);
            assert!(c.cache_set(1, 100).await.unwrap().is_none());
            assert!(c.cache_get(&1).await.unwrap().is_some());

            sleep(Duration::new(1, 600000));
            assert!(c.cache_get(&1).await.unwrap().is_none());

            c.cache_set_lifespan(10).unwrap();
            assert!(c.cache_set(1, 100).await.unwrap().is_none());
            assert!(c.cache_set(2, 100).await.unwrap().is_none());
            assert_eq!(c.cache_get(&1).await.unwrap().unwrap(), 100);
            assert_eq!(c.cache_get(&1).await.unwrap().unwrap(), 100);
        }
    }
}

#[cfg(all(
    feature = "async",
    any(feature = "disk_async_std", feature = "disk_tokio")
))]
pub use async_disk::{AsyncDiskCache, AsyncDiskCacheBuilder};
*/

#[cfg(test)]
/// Cache store tests
mod tests {
    use std::thread::sleep;
    use std::time::Duration;

    use super::*;

    fn now_millis() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    }

    #[test]
    fn disk_cache_set_get_remove() {
        let cache: DiskCache<u32, u32> =
            DiskCache::new(format!("{}:disk-cache-test-sgr", now_millis()), 3600)
                .set_disk_path(std::env::temp_dir().join("cachedtest-sgr"))
                .build()
                .unwrap();

        let cached = cache.cache_get(&6).unwrap();
        assert!(cached.is_none());

        let cached = cache.cache_set(6, 4444).unwrap();
        assert_eq!(cached, None);

        let cached = cache.cache_set(6, 5555).unwrap();
        assert_eq!(cached, Some(4444));

        let cached = cache.cache_get(&6).unwrap();
        assert_eq!(cached, Some(5555));

        let cached = cache.cache_remove(&6).unwrap();
        assert_eq!(cached, Some(5555));

        let cached = cache.cache_get(&6).unwrap();
        assert!(cached.is_none());

        drop(cache);
    }

    #[test]
    fn disk_cache() {
        let mut c: DiskCache<u32, u32> =
            DiskCache::new(format!("{}:disk-cache-test", now_millis()), 2)
                .set_namespace("in-tests:")
                .build()
                .unwrap();

        assert!(c.cache_get(&1).unwrap().is_none());

        assert!(c.cache_set(1, 100).unwrap().is_none());
        assert!(c.cache_get(&1).unwrap().is_some());

        sleep(Duration::new(2, 500000));
        assert!(c.cache_get(&1).unwrap().is_none());

        let old = c.cache_set_lifespan(1).unwrap();
        assert_eq!(2, old);
        assert!(c.cache_set(1, 100).unwrap().is_none());
        assert!(c.cache_get(&1).unwrap().is_some());

        sleep(Duration::new(1, 600000));
        assert!(c.cache_get(&1).unwrap().is_none());

        c.cache_set_lifespan(10).unwrap();
        assert!(c.cache_set(1, 100).unwrap().is_none());
        assert!(c.cache_set(2, 100).unwrap().is_none());
        assert_eq!(c.cache_get(&1).unwrap().unwrap(), 100);
        assert_eq!(c.cache_get(&1).unwrap().unwrap(), 100);
    }

    #[test]
    fn remove() {
        let cache: DiskCache<u32, u32> =
            DiskCache::new(format!("{}:disk-cache-test-remove", now_millis()), 3600)
                .set_disk_path(std::env::temp_dir().join("cachedtest-remove"))
                .build()
                .unwrap();

        assert!(cache.cache_set(1, 100).unwrap().is_none());
        assert!(cache.cache_set(2, 200).unwrap().is_none());
        assert!(cache.cache_set(3, 300).unwrap().is_none());

        assert_eq!(100, cache.cache_remove(&1).unwrap().unwrap());

        drop(cache);
    }
}
