use crate::IOCached;
use directories::BaseDirs;
use instant::Duration;
use serde::de::DeserializeOwned;
use serde::Serialize;
use sled::Db;
use std::marker::PhantomData;
use std::path::Path;
use std::{fmt::Display, path::PathBuf, time::SystemTime};

pub struct DiskCacheBuilder<K, V> {
    seconds: Option<u64>,
    refresh: bool,
    disk_dir: Option<PathBuf>,
    cache_name: String,
    _phantom: PhantomData<(K, V)>,
}

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

static DISK_FILE_PREFIX: &str = "cached_disk_cache";
const DISK_FILE_VERSION: u64 = 1;

impl<K, V> DiskCacheBuilder<K, V>
where
    K: Display,
    V: Serialize + DeserializeOwned,
{
    /// Initialize a `DiskCacheBuilder`
    pub fn new<S: AsRef<str>>(cache_name: S) -> DiskCacheBuilder<K, V> {
        Self {
            seconds: None,
            refresh: false,
            disk_dir: None,
            cache_name: cache_name.as_ref().to_string(),
            _phantom: Default::default(),
        }
    }

    /// Specify the cache TTL/lifespan in seconds
    pub fn set_lifespan(mut self, seconds: u64) -> Self {
        self.seconds = Some(seconds);
        self
    }

    /// Specify whether cache hits refresh the TTL
    pub fn set_refresh(mut self, refresh: bool) -> Self {
        self.refresh = refresh;
        self
    }

    /// Set the disk path for where the data will be stored
    pub fn set_disk_directory<P: AsRef<Path>>(mut self, dir: P) -> Self {
        self.disk_dir = Some(dir.as_ref().into());
        self
    }

    fn default_disk_dir() -> PathBuf {
        BaseDirs::new()
            .map(|base_dirs| {
                let exe_name = std::env::current_exe()
                    .ok()
                    .and_then(|path| {
                        path.file_name()
                            .and_then(|os_str| os_str.to_str().map(|s| format!("{}_", s)))
                    })
                    .unwrap_or_default();
                let dir_prefix = format!("{}{}", exe_name, DISK_FILE_PREFIX);
                base_dirs.cache_dir().join(dir_prefix)
            })
            .unwrap_or_else(|| {
                std::env::current_dir().expect("disk cache unable to determine current directory")
            })
    }

    pub fn build(self) -> Result<DiskCache<K, V>, DiskCacheBuildError> {
        let disk_dir = self.disk_dir.unwrap_or_else(|| Self::default_disk_dir());
        let disk_path = disk_dir.join(format!("{}_v{}", self.cache_name, DISK_FILE_VERSION));
        let connection = sled::open(disk_path.clone())?;

        Ok(DiskCache {
            seconds: self.seconds,
            refresh: self.refresh,
            version: DISK_FILE_VERSION,
            disk_path,
            connection,
            _phantom: self._phantom,
        })
    }
}

/// Cache store backed by disk
pub struct DiskCache<K, V> {
    pub(super) seconds: Option<u64>,
    pub(super) refresh: bool,
    #[allow(unused)]
    version: u64,
    #[allow(unused)]
    disk_path: PathBuf,
    connection: Db,
    _phantom: PhantomData<(K, V)>,
}

impl<K, V> DiskCache<K, V>
where
    K: Display,
    V: Serialize + DeserializeOwned,
{
    #[allow(clippy::new_ret_no_self)]
    /// Initialize a `DiskCacheBuilder`
    pub fn new(cache_name: &str) -> DiskCacheBuilder<K, V> {
        DiskCacheBuilder::new(cache_name)
    }

    pub fn remove_expired_entries(&self) {
        let now = SystemTime::now();

        for (key, value) in self.connection.iter().flatten() {
            if let Ok(cached) = rmp_serde::from_slice::<CachedDiskValue<V>>(&value) {
                if let Some(lifetime_seconds) = self.seconds {
                    if now
                        .duration_since(cached.created_at)
                        .unwrap_or(Duration::from_secs(0))
                        < Duration::from_secs(lifetime_seconds)
                    {
                        let _ = self.connection.remove(key);
                    }
                }
            }
        }
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
    pub(crate) created_at: SystemTime,
    pub(crate) version: u64,
}

impl<V> CachedDiskValue<V> {
    fn new(value: V) -> Self {
        Self {
            value,
            created_at: SystemTime::now(),
            version: 1,
        }
    }

    fn refresh_created_at(&mut self) {
        self.created_at = SystemTime::now();
    }
}

impl<K, V> IOCached<K, V> for DiskCache<K, V>
where
    K: Display,
    V: Serialize + DeserializeOwned,
{
    type Error = DiskCacheError;

    fn cache_get(&self, key: &K) -> Result<Option<V>, DiskCacheError> {
        let key = key.to_string();
        let seconds = self.seconds;
        let refresh = self.refresh;
        let update = |old: Option<&[u8]>| -> Option<Vec<u8>> {
            let old = old?;
            if seconds.is_none() {
                return Some(old.to_vec());
            }
            let seconds = seconds.unwrap();
            let mut cached = match rmp_serde::from_slice::<CachedDiskValue<V>>(old) {
                Ok(cached) => cached,
                Err(_) => {
                    // unable to deserialize, treat it as not existing
                    return None;
                }
            };
            if SystemTime::now()
                .duration_since(cached.created_at)
                .unwrap_or(Duration::from_secs(0))
                < Duration::from_secs(seconds)
            {
                if refresh {
                    cached.refresh_created_at();
                }
                let cache_val =
                    rmp_serde::to_vec(&cached).expect("error serializing cached disk value");
                Some(cache_val)
            } else {
                None
            }
        };

        if let Some(data) = self.connection.update_and_fetch(key, update)? {
            let cached = rmp_serde::from_slice::<CachedDiskValue<V>>(&data)?;
            Ok(Some(cached.value))
        } else {
            Ok(None)
        }
    }

    fn cache_set(&self, key: K, value: V) -> Result<Option<V>, DiskCacheError> {
        let key = key.to_string();
        let value = rmp_serde::to_vec(&CachedDiskValue::new(value))?;

        if let Some(data) = self.connection.insert(key, value)? {
            let cached = rmp_serde::from_slice::<CachedDiskValue<V>>(&data)?;

            if let Some(lifetime_seconds) = self.seconds {
                if SystemTime::now()
                    .duration_since(cached.created_at)
                    .unwrap_or(Duration::from_secs(0))
                    < Duration::from_secs(lifetime_seconds)
                {
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
        let key = key.to_string();
        if let Some(data) = self.connection.remove(key)? {
            let cached = rmp_serde::from_slice::<CachedDiskValue<V>>(&data)?;

            if let Some(lifetime_seconds) = self.seconds {
                if SystemTime::now()
                    .duration_since(cached.created_at)
                    .unwrap_or(Duration::from_secs(0))
                    < Duration::from_secs(lifetime_seconds)
                {
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
        self.seconds
    }

    fn cache_set_lifespan(&mut self, seconds: u64) -> Option<u64> {
        let old = self.seconds;
        self.seconds = Some(seconds);
        old
    }

    fn cache_set_refresh(&mut self, refresh: bool) -> bool {
        let old = self.refresh;
        self.refresh = refresh;
        old
    }
}

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
    fn disk_set_get_remove() {
        let cache: DiskCache<u32, u32> =
            DiskCache::new(&format!("{}:disk-cache-test-sgr", now_millis()))
                .set_disk_directory(std::env::temp_dir().join("cachedtest-sgr"))
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
    fn disk_expire() {
        let mut c: DiskCache<u32, u32> =
            DiskCache::new(&format!("{}:disk-cache-test", now_millis()))
                .set_lifespan(2)
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
    fn disk_remove() {
        let cache: DiskCache<u32, u32> =
            DiskCache::new(&format!("{}:disk-cache-test-remove", now_millis()))
                .set_disk_directory(std::env::temp_dir().join("cachedtest-remove"))
                .build()
                .unwrap();

        assert!(cache.cache_set(1, 100).unwrap().is_none());
        assert!(cache.cache_set(2, 200).unwrap().is_none());
        assert!(cache.cache_set(3, 300).unwrap().is_none());

        assert_eq!(100, cache.cache_remove(&1).unwrap().unwrap());

        drop(cache);
    }

    #[test]
    fn disk_default_cache_dir() {
        let cache: DiskCache<u32, u32> =
            DiskCache::new(&format!("{}:disk-cache-test-default-dir", now_millis()))
                .build()
                .unwrap();

        assert!(cache.cache_set(1, 100).unwrap().is_none());
        assert!(cache.cache_set(2, 200).unwrap().is_none());
        assert!(cache.cache_set(3, 300).unwrap().is_none());

        assert_eq!(100, cache.cache_remove(&1).unwrap().unwrap());

        drop(cache);
    }
}
