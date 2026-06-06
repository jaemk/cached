#[cfg(feature = "time_stores")]
use crate::CacheTtl;
use crate::ConcurrentCached;
use crate::time::Duration;
use crate::time::SystemTime;
use directories::BaseDirs;
use parking_lot::Mutex;
use serde::Serialize;
use serde::de::DeserializeOwned;
use sled::Db;
use std::io::ErrorKind;
use std::marker::PhantomData;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

pub struct DiskCacheBuilder<K, V> {
    ttl: Option<Duration>,
    refresh: bool,
    sync_to_disk_on_cache_change: bool,
    disk_dir: Option<PathBuf>,
    cache_name: String,
    connection_config: Option<sled::Config>,
    // fn-pointer phantom — see the rationale on `DiskCache::_phantom`; keeps the
    // type unconditionally `Send + Sync` regardless of `K`/`V`.
    _phantom: PhantomData<fn() -> (K, V)>,
}

use thiserror::Error;

#[derive(Error, Debug)]
pub enum DiskCacheBuildError {
    #[error("Storage connection error")]
    ConnectionError(#[from] sled::Error),
    #[error(transparent)]
    InvalidTtl(#[from] super::BuildError),
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
    K: ToString,
    V: Serialize + DeserializeOwned,
{
    /// Initialize a `DiskCacheBuilder`
    pub fn new<S: AsRef<str>>(cache_name: S) -> DiskCacheBuilder<K, V> {
        Self {
            ttl: None,
            refresh: false,
            sync_to_disk_on_cache_change: false,
            disk_dir: None,
            cache_name: cache_name.as_ref().to_string(),
            connection_config: None,
            _phantom: Default::default(),
        }
    }

    /// Specify the cache TTL as a `Duration`.
    pub fn ttl(mut self, ttl: Duration) -> Self {
        self.ttl = Some(ttl);
        self
    }

    /// Specify whether cache hits refresh the TTL
    pub fn refresh(mut self, refresh: bool) -> Self {
        self.refresh = refresh;
        self
    }

    /// Set the disk path for where the data will be stored
    pub fn disk_directory<P: AsRef<Path>>(mut self, dir: P) -> Self {
        self.disk_dir = Some(dir.as_ref().into());
        self
    }

    /// Specify whether the cache should sync to disk on each cache change.
    /// [sled] flushes every [sled::Config::flush_every_ms] which has a default value.
    /// In some use cases, the default value may not be quick enough,
    /// or a user may want to reduce the flush rate / turn off auto-flushing to reduce IO (and only flush on cache changes).
    /// (see [DiskCacheBuilder::connection_config] for more control over the sled connection)
    pub fn sync_to_disk_on_cache_change(mut self, sync_to_disk_on_cache_change: bool) -> Self {
        self.sync_to_disk_on_cache_change = sync_to_disk_on_cache_change;
        self
    }

    /// Specify the [sled::Config] to use for the connection to the disk cache.
    ///
    /// ### Note
    /// Don't use [sled::Config::path] as any value set here will be overwritten by either
    /// the path specified in [DiskCacheBuilder::disk_directory], or the default value calculated by [DiskCacheBuilder].
    ///
    /// ### Example Use Case
    /// By default [sled] automatically syncs to disk at a frequency specified in [sled::Config::flush_every_ms].
    /// A user may want to reduce IO by setting a lower flush frequency, or by setting [sled::Config::flush_every_ms] to [None].
    /// Also see [DiskCacheBuilder::sync_to_disk_on_cache_change] which allows for syncing to disk on each cache change.
    /// ```rust,no_run
    /// use cached::stores::{DiskCacheBuilder, DiskCache};
    ///
    /// let config = sled::Config::new().flush_every_ms(None);
    /// let cache: DiskCache<String, String> = DiskCacheBuilder::new("my-cache")
    ///     .connection_config(config)
    ///     .sync_to_disk_on_cache_change(true)
    ///     .build()
    ///     .unwrap();
    /// ```
    pub fn connection_config(mut self, config: sled::Config) -> Self {
        self.connection_config = Some(config);
        self
    }

    fn default_disk_dir_candidates() -> Vec<PathBuf> {
        let exe_name = std::env::current_exe()
            .ok()
            .and_then(|path| {
                path.file_name()
                    .and_then(|os_str| os_str.to_str().map(|s| format!("{}_", s)))
            })
            .unwrap_or_default();
        let dir_prefix = format!("{}{}", exe_name, DISK_FILE_PREFIX);
        let mut candidates = Vec::new();

        if let Some(base_dirs) = BaseDirs::new() {
            candidates.push(base_dirs.cache_dir().join(&dir_prefix));
        }

        candidates.push(std::env::temp_dir().join(dir_prefix));
        candidates
    }

    fn try_open(config: Option<sled::Config>, disk_path: PathBuf) -> Result<Db, sled::Error> {
        match config {
            Some(config) => config.path(disk_path).open(),
            None => sled::open(disk_path),
        }
    }

    fn default_disk_path(cache_dir_name: &str) -> Result<PathBuf, sled::Error> {
        let mut last_error = None;

        for disk_dir in Self::default_disk_dir_candidates() {
            let disk_path = disk_dir.join(cache_dir_name);
            match std::fs::create_dir_all(&disk_path) {
                Ok(()) => return Ok(disk_path),
                Err(error) if error.kind() == ErrorKind::PermissionDenied => {
                    last_error = Some(error);
                }
                Err(error) => return Err(sled::Error::Io(error)),
            }
        }

        Err(sled::Error::Io(last_error.unwrap_or_else(|| {
            std::io::Error::new(
                ErrorKind::PermissionDenied,
                "unable to create a writable default disk cache directory",
            )
        })))
    }

    pub fn build(self) -> Result<DiskCache<K, V>, DiskCacheBuildError> {
        if let Some(ttl) = self.ttl {
            super::validate_ttl(ttl)?;
        }
        let cache_dir_name = format!("{}_v{}", self.cache_name, DISK_FILE_VERSION);

        let (disk_path, connection) = if let Some(disk_dir) = self.disk_dir {
            let disk_path = disk_dir.join(&cache_dir_name);
            let connection = Self::try_open(self.connection_config, disk_path.clone())?;
            (disk_path, connection)
        } else {
            let disk_path = Self::default_disk_path(&cache_dir_name)?;
            let connection = Self::try_open(self.connection_config, disk_path.clone())?;
            (disk_path, connection)
        };

        Ok(DiskCache {
            ttl: Mutex::new(self.ttl),
            refresh: AtomicBool::new(self.refresh),
            sync_to_disk_on_cache_change: self.sync_to_disk_on_cache_change,
            version: DISK_FILE_VERSION,
            disk_path,
            connection,
            _phantom: self._phantom,
        })
    }
}

/// Cache store backed by disk
pub struct DiskCache<K, V> {
    pub(super) ttl: Mutex<Option<Duration>>,
    pub(super) refresh: AtomicBool,
    sync_to_disk_on_cache_change: bool,
    #[allow(unused)]
    version: u64,
    #[allow(unused)]
    disk_path: PathBuf,
    connection: Db,
    // `DiskCache`/`DiskCacheBuilder` own no live `K`/`V` (values are serialized
    // to disk; `K`/`V` only appear in method signatures). Use a fn-pointer
    // phantom so the type is unconditionally `Send + Sync` and does not impose
    // `K: Sync`/`V: Sync` on callers (e.g. the async impl). Variance is
    // unchanged: covariant in `K` and `V`, same as `PhantomData<(K, V)>`.
    _phantom: PhantomData<fn() -> (K, V)>,
}

impl<K, V> DiskCache<K, V>
where
    K: ToString,
    V: Serialize + DeserializeOwned,
{
    #[allow(clippy::new_ret_no_self)]
    /// Initialize a `DiskCacheBuilder`
    pub fn new(cache_name: &str) -> DiskCacheBuilder<K, V> {
        DiskCacheBuilder::new(cache_name)
    }

    /// Initialize a `DiskCacheBuilder`.
    pub fn builder(cache_name: &str) -> DiskCacheBuilder<K, V> {
        DiskCacheBuilder::new(cache_name)
    }

    pub fn remove_expired_entries(&self) -> Result<(), DiskCacheError> {
        let now = SystemTime::now();

        let ttl = *self.ttl.lock();
        for item in self.connection.iter() {
            let (key, value) = item?;
            let cached = rmp_serde::from_slice::<CachedDiskValue<V>>(&value)?;
            if let Some(ttl) = ttl {
                if now
                    .duration_since(cached.created_at)
                    .unwrap_or(Duration::from_secs(0))
                    >= ttl
                {
                    self.connection.remove(key)?;
                }
            }
        }

        if self.sync_to_disk_on_cache_change {
            self.connection.flush()?;
        }
        Ok(())
    }

    /// Provide access to the underlying [sled::Db] connection
    /// This is useful for i.e. manually flushing the cache to disk.
    pub fn connection(&self) -> &Db {
        &self.connection
    }

    /// Provide mutable access to the underlying [sled::Db] connection
    pub fn connection_mut(&mut self) -> &mut Db {
        &mut self.connection
    }
}

#[derive(Error, Debug)]
pub enum DiskCacheError {
    #[error("Storage error")]
    StorageError(#[from] sled::Error),
    #[error("Error deserializing cached value")]
    CacheDeserializationError(#[from] rmp_serde::decode::Error),
    #[error("Error serializing cached value")]
    CacheSerializationError(#[from] rmp_serde::encode::Error),
    /// The blocking task used to run `sled` I/O off the async runtime was
    /// cancelled or panicked. Only produced by the async
    /// (`ConcurrentCachedAsync`) path.
    ///
    /// Effectively unreachable in normal operation: the blocking work is itself
    /// fallible and returns the variants above, so this surfaces only if the
    /// Tokio runtime aborts/cancels the blocking task (e.g. runtime shutdown).
    /// The underlying `JoinError` is intentionally not carried, to keep
    /// `tokio` out of this (sync-shared) public error type.
    #[error("disk cache background task failed")]
    BackgroundTaskFailed,
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

// ── Connection-level disk operations ─────────────────────────────────────────
//
// These free functions hold the single source of truth for the on-disk
// behavior (TTL/refresh handling, serialization-error propagation, optional
// flush). The synchronous `ConcurrentCached` impl calls them directly; the async
// `ConcurrentCachedAsync` impl calls them inside `tokio::task::spawn_blocking` so the
// blocking `sled` I/O does not stall the async runtime. Keeping one
// implementation guarantees the sync and async paths stay behaviorally
// identical.

fn disk_cache_get<V>(
    connection: &Db,
    key: &str,
    ttl: Option<Duration>,
    refresh: bool,
    sync_to_disk_on_cache_change: bool,
) -> Result<Option<V>, DiskCacheError>
where
    V: Serialize + DeserializeOwned,
{
    let Some(data) = connection.get(key)? else {
        return Ok(None);
    };

    let mut cached = rmp_serde::from_slice::<CachedDiskValue<V>>(&data)?;

    if let Some(ttl) = ttl {
        if SystemTime::now()
            .duration_since(cached.created_at)
            .unwrap_or(Duration::from_secs(0))
            < ttl
        {
            if refresh {
                cached.refresh_created_at();
                connection.insert(key, rmp_serde::to_vec(&cached)?)?;
                if sync_to_disk_on_cache_change {
                    connection.flush()?;
                }
            }
            Ok(Some(cached.value))
        } else {
            connection.remove(key)?;
            if sync_to_disk_on_cache_change {
                connection.flush()?;
            }
            Ok(None)
        }
    } else {
        Ok(Some(cached.value))
    }
}

fn disk_cache_set<V>(
    connection: &Db,
    key: &str,
    serialized: Vec<u8>,
    sync_to_disk_on_cache_change: bool,
) -> Result<Option<V>, DiskCacheError>
where
    V: DeserializeOwned,
{
    let result = if let Some(data) = connection.insert(key, serialized)? {
        let cached = rmp_serde::from_slice::<CachedDiskValue<V>>(&data)?;
        Ok(Some(cached.value))
    } else {
        Ok(None)
    };

    if sync_to_disk_on_cache_change {
        connection.flush()?;
    }

    result
}

fn disk_cache_remove<V>(
    connection: &Db,
    key: &str,
    ttl: Option<Duration>,
    sync_to_disk_on_cache_change: bool,
) -> Result<Option<V>, DiskCacheError>
where
    V: DeserializeOwned,
{
    let result = if let Some(data) = connection.remove(key)? {
        let cached = rmp_serde::from_slice::<CachedDiskValue<V>>(&data)?;

        if let Some(ttl) = ttl {
            if SystemTime::now()
                .duration_since(cached.created_at)
                .unwrap_or(Duration::from_secs(0))
                < ttl
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
    };

    if sync_to_disk_on_cache_change {
        connection.flush()?;
    }

    result
}

fn disk_cache_remove_entry<V>(
    connection: &Db,
    key: &str,
    sync_to_disk_on_cache_change: bool,
) -> Result<Option<V>, DiskCacheError>
where
    V: DeserializeOwned,
{
    let result = if let Some(data) = connection.remove(key)? {
        let cached = rmp_serde::from_slice::<CachedDiskValue<V>>(&data)?;
        Ok(Some(cached.value))
    } else {
        Ok(None)
    };

    if sync_to_disk_on_cache_change {
        connection.flush()?;
    }

    result
}

fn disk_cache_delete(
    connection: &Db,
    key: &str,
    sync_to_disk_on_cache_change: bool,
) -> Result<bool, DiskCacheError> {
    let removed = connection.remove(key)?.is_some();

    if sync_to_disk_on_cache_change {
        connection.flush()?;
    }

    Ok(removed)
}

impl<K, V> ConcurrentCached<K, V> for DiskCache<K, V>
where
    K: ToString + Clone,
    V: Serialize + DeserializeOwned,
{
    type Error = DiskCacheError;

    fn cache_get(&self, key: &K) -> Result<Option<V>, DiskCacheError> {
        let ttl = *self.ttl.lock();
        let refresh = self.refresh.load(Ordering::Relaxed);
        disk_cache_get(
            &self.connection,
            &key.to_string(),
            ttl,
            refresh,
            self.sync_to_disk_on_cache_change,
        )
    }

    fn cache_set(&self, key: K, value: V) -> Result<Option<V>, DiskCacheError> {
        let serialized = rmp_serde::to_vec(&CachedDiskValue::new(value))?;
        disk_cache_set(
            &self.connection,
            &key.to_string(),
            serialized,
            self.sync_to_disk_on_cache_change,
        )
    }

    fn cache_remove(&self, key: &K) -> Result<Option<V>, DiskCacheError> {
        let ttl = *self.ttl.lock();
        disk_cache_remove(
            &self.connection,
            &key.to_string(),
            ttl,
            self.sync_to_disk_on_cache_change,
        )
    }

    fn cache_remove_entry(&self, key: &K) -> Result<Option<(K, V)>, Self::Error> {
        disk_cache_remove_entry(
            &self.connection,
            &key.to_string(),
            self.sync_to_disk_on_cache_change,
        )
        .map(|opt| opt.map(|v| (key.clone(), v)))
    }

    fn cache_delete(&self, key: &K) -> Result<bool, DiskCacheError> {
        disk_cache_delete(
            &self.connection,
            &key.to_string(),
            self.sync_to_disk_on_cache_change,
        )
    }

    fn ttl(&self) -> Option<Duration> {
        *self.ttl.lock()
    }

    fn set_ttl(&self, ttl: Duration) -> Option<Duration> {
        self.ttl.lock().replace(ttl)
    }

    fn set_refresh_on_hit(&self, refresh: bool) -> bool {
        self.refresh.swap(refresh, Ordering::Relaxed)
    }

    fn unset_ttl(&self) -> Option<Duration> {
        self.ttl.lock().take()
    }
}

/// Async disk cache. `sled` has no async API, so every operation is run on
/// `tokio`'s blocking thread pool via [`tokio::task::spawn_blocking`] to avoid
/// stalling the async runtime. Behavior is identical to the synchronous
/// [`ConcurrentCached`] impl (they share the `disk_cache_*` helpers).
///
/// Values need only be `Send`, **not `Sync`**: they are serialized before the
/// work moves onto the blocking pool, so no `V` is held across the `.await`
/// (only the owned serialized bytes / the `JoinHandle<Result<Option<V>, _>>`).
/// Keys keep `Send + Sync` (the `&K` is borrowed across the await), consistent
/// with the `RedisCache`/`AsyncRedisCache` async stores.
///
/// Cancellation: dropping the returned future does **not** cancel the in-flight
/// `spawn_blocking` `sled` operation — it runs to completion on the blocking
/// pool (only the result is discarded). This is safe for a cache (`sled`
/// operations are atomic, so no corruption), but a cancelled `cache_set`/
/// `cache_remove` may still have taken effect on disk.
///
/// **Concurrency note:** each call spawns a new blocking task on tokio's blocking
/// thread pool (default limit: 512 threads). Under high concurrency this pool can
/// saturate, causing subsequent `spawn_blocking` calls to queue. If your workload
/// issues many concurrent disk-cache operations, tune the pool with
/// `tokio::runtime::Builder::max_blocking_threads` or consider an explicit
/// rate-limiting layer above the cache.
#[cfg(feature = "async")]
#[cfg_attr(docsrs, doc(cfg(feature = "async")))]
impl<K, V> crate::ConcurrentCachedAsync<K, V> for DiskCache<K, V>
where
    K: ToString + Clone + Send + Sync,
    V: Serialize + DeserializeOwned + Send + 'static,
{
    type Error = DiskCacheError;

    async fn cache_get(&self, key: &K) -> Result<Option<V>, DiskCacheError> {
        let connection = self.connection.clone();
        let key = key.to_string();
        let (ttl, refresh, sync) = (
            *self.ttl.lock(),
            self.refresh.load(Ordering::Relaxed),
            self.sync_to_disk_on_cache_change,
        );
        tokio::task::spawn_blocking(move || {
            disk_cache_get::<V>(&connection, &key, ttl, refresh, sync)
        })
        .await
        .map_err(|_| DiskCacheError::BackgroundTaskFailed)?
    }

    async fn cache_set(&self, key: K, value: V) -> Result<Option<V>, DiskCacheError> {
        let connection = self.connection.clone();
        let key = key.to_string();
        let sync = self.sync_to_disk_on_cache_change;
        let serialized = rmp_serde::to_vec(&CachedDiskValue::new(value))?;
        tokio::task::spawn_blocking(move || {
            disk_cache_set::<V>(&connection, &key, serialized, sync)
        })
        .await
        .map_err(|_| DiskCacheError::BackgroundTaskFailed)?
    }

    async fn cache_remove(&self, key: &K) -> Result<Option<V>, DiskCacheError> {
        let connection = self.connection.clone();
        let key = key.to_string();
        let (ttl, sync) = (*self.ttl.lock(), self.sync_to_disk_on_cache_change);
        tokio::task::spawn_blocking(move || disk_cache_remove::<V>(&connection, &key, ttl, sync))
            .await
            .map_err(|_| DiskCacheError::BackgroundTaskFailed)?
    }

    async fn cache_remove_entry(&self, key: &K) -> Result<Option<(K, V)>, Self::Error> {
        let connection = self.connection.clone();
        let key_str = key.to_string();
        let sync = self.sync_to_disk_on_cache_change;
        let v: Option<V> = tokio::task::spawn_blocking(move || {
            disk_cache_remove_entry::<V>(&connection, &key_str, sync)
        })
        .await
        .map_err(|_| DiskCacheError::BackgroundTaskFailed)??;
        Ok(v.map(|v| (key.clone(), v)))
    }

    async fn cache_delete(&self, key: &K) -> Result<bool, DiskCacheError> {
        let connection = self.connection.clone();
        let key = key.to_string();
        let sync = self.sync_to_disk_on_cache_change;
        tokio::task::spawn_blocking(move || disk_cache_delete(&connection, &key, sync))
            .await
            .map_err(|_| DiskCacheError::BackgroundTaskFailed)?
    }

    fn set_refresh_on_hit(&self, refresh: bool) -> bool {
        self.refresh.swap(refresh, Ordering::Relaxed)
    }

    fn ttl(&self) -> Option<Duration> {
        *self.ttl.lock()
    }

    fn set_ttl(&self, ttl: Duration) -> Option<Duration> {
        self.ttl.lock().replace(ttl)
    }

    fn unset_ttl(&self) -> Option<Duration> {
        self.ttl.lock().take()
    }
}

#[cfg(feature = "time_stores")]
impl<K, V> CacheTtl for DiskCache<K, V> {
    fn ttl(&self) -> Option<Duration> {
        *self.ttl.lock()
    }
    fn set_ttl(&mut self, ttl: Duration) -> Option<Duration> {
        self.ttl.lock().replace(ttl)
    }
    fn unset_ttl(&mut self) -> Option<Duration> {
        self.ttl.lock().take()
    }
    fn refresh_on_hit(&self) -> bool {
        self.refresh.load(Ordering::Relaxed)
    }
    fn set_refresh_on_hit(&mut self, refresh: bool) -> bool {
        self.refresh.swap(refresh, Ordering::Relaxed)
    }
}

#[cfg(test)]
#[allow(non_snake_case)]
mod test_DiskCache {
    use crate::time::Duration;
    use googletest::{
        GoogleTestSupport as _, assert_that,
        matchers::{anything, eq, none, ok, some},
    };
    use std::thread::sleep;
    use tempfile::TempDir;

    use super::*;

    /// If passing `no_exist` to the macro:
    /// This gives you a TempDir where the directory does not exist
    /// so you can copy / move things to the returned TmpDir.path()
    /// and those files will be removed when the TempDir is dropped
    macro_rules! temp_dir {
        () => {
            TempDir::new().expect("Error creating temp dir")
        };
        (no_exist) => {{
            let tmp_dir = TempDir::new().expect("Error creating temp dir");
            std::fs::remove_dir_all(tmp_dir.path()).expect("error emptying the tmp dir");
            tmp_dir
        }};
    }

    fn now_millis() -> u128 {
        crate::time::SystemTime::now()
            .duration_since(crate::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    }

    #[derive(Debug)]
    struct SerializeFailsAfterDeserialize {
        fail: bool,
    }

    impl serde::Serialize for SerializeFailsAfterDeserialize {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            if self.fail {
                Err(serde::ser::Error::custom("intentional serialize failure"))
            } else {
                serializer.serialize_bool(false)
            }
        }
    }

    impl<'de> serde::Deserialize<'de> for SerializeFailsAfterDeserialize {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            let _ = bool::deserialize(deserializer)?;
            Ok(Self { fail: true })
        }
    }

    const TEST_KEY: u32 = 1;
    const TEST_VAL: u32 = 100;
    const TEST_VAL_1: u32 = 200;

    #[test]
    fn cache_get_returns_serialize_error_when_refresh_fails() {
        let tmp_dir = temp_dir!();
        let cache: DiskCache<u32, SerializeFailsAfterDeserialize> =
            DiskCache::new("serialize_error_on_refresh")
                .disk_directory(tmp_dir.path())
                .ttl(Duration::from_secs(10))
                .refresh(true)
                .build()
                .expect("error building disk cache");
        let cached = CachedDiskValue::new(SerializeFailsAfterDeserialize { fail: false });
        cache
            .connection
            .insert(
                TEST_KEY.to_string(),
                rmp_serde::to_vec(&cached).expect("error serializing fixture"),
            )
            .expect("error inserting fixture");

        assert!(matches!(
            cache.cache_get(&TEST_KEY),
            Err(DiskCacheError::CacheSerializationError(_))
        ));
    }

    #[test]
    fn cache_get_returns_decode_error_for_corrupted_value() {
        let tmp_dir = temp_dir!();
        let cache: DiskCache<u32, u32> = DiskCache::new("corrupted-cache-get")
            .disk_directory(tmp_dir.path())
            .build()
            .expect("error building disk cache");
        cache
            .connection
            .insert(TEST_KEY.to_string(), vec![0xc1, 0xc1, 0xc1])
            .expect("error inserting corrupt fixture");

        assert!(matches!(
            cache.cache_get(&TEST_KEY),
            Err(DiskCacheError::CacheDeserializationError(_))
        ));
        assert!(
            cache
                .connection
                .get(TEST_KEY.to_string())
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn cache_delete_removes_corrupted_value_without_decoding() {
        let tmp_dir = temp_dir!();
        let cache: DiskCache<u32, u32> = DiskCache::new("corrupted-cache-delete")
            .disk_directory(tmp_dir.path())
            .build()
            .expect("error building disk cache");
        cache
            .connection
            .insert(TEST_KEY.to_string(), vec![0xc1, 0xc1, 0xc1])
            .expect("error inserting corrupt fixture");

        assert!(cache.cache_delete(&TEST_KEY).unwrap());
        assert!(!cache.cache_delete(&TEST_KEY).unwrap());
        assert_that!(cache.cache_get(&TEST_KEY), ok(none()));
    }

    #[test]
    fn remove_expired_entries_returns_decode_error_for_corrupted_value() {
        let tmp_dir = temp_dir!();
        let cache: DiskCache<u32, u32> = DiskCache::new("corrupted-sweep")
            .disk_directory(tmp_dir.path())
            .ttl(Duration::from_secs(1))
            .build()
            .expect("error building disk cache");
        cache
            .connection
            .insert(TEST_KEY.to_string(), vec![0xc1, 0xc1, 0xc1])
            .expect("error inserting corrupt fixture");

        assert!(matches!(
            cache.remove_expired_entries(),
            Err(DiskCacheError::CacheDeserializationError(_))
        ));
    }

    const LIFE_SPAN_2_SECS: Duration = Duration::from_secs(2);
    const LIFE_SPAN_1_SEC: Duration = Duration::from_secs(1);
    #[googletest::test]
    fn cache_get_after_cache_remove_returns_none() {
        let tmp_dir = temp_dir!();
        let cache: DiskCache<u32, u32> = DiskCache::new("test-cache")
            .disk_directory(tmp_dir.path())
            .build()
            .unwrap();

        let cached = cache.cache_get(&TEST_KEY).unwrap();
        assert_that!(
            cached,
            none(),
            "Getting a non-existent key-value should return None"
        );

        let cached = cache.cache_set(TEST_KEY, TEST_VAL).unwrap();
        assert_that!(cached, none(), "Setting a new key-value should return None");

        let cached = cache.cache_set(TEST_KEY, TEST_VAL_1).unwrap();
        assert_that!(
            cached,
            some(eq(TEST_VAL)),
            "Setting an existing key-value should return the old value"
        );

        let cached = cache.cache_get(&TEST_KEY).unwrap();
        assert_that!(
            cached,
            some(eq(TEST_VAL_1)),
            "Getting an existing key-value should return the value"
        );

        let cached = cache.cache_remove(&TEST_KEY).unwrap();
        assert_that!(
            cached,
            some(eq(TEST_VAL_1)),
            "Removing an existing key-value should return the value"
        );

        let cached = cache.cache_get(&TEST_KEY).unwrap();
        assert_that!(cached, none(), "Getting a removed key should return None");

        drop(cache);
    }

    #[googletest::test]
    fn values_expire_when_lifespan_elapses_returning_none() {
        let tmp_dir = temp_dir!();
        let cache: DiskCache<u32, u32> = DiskCache::new("test-cache")
            .disk_directory(tmp_dir.path())
            .ttl(LIFE_SPAN_2_SECS)
            .build()
            .unwrap();

        assert_that!(
            cache.cache_get(&TEST_KEY),
            ok(none()),
            "Getting a non-existent key-value should return None"
        );

        assert_that!(
            cache.cache_set(TEST_KEY, 100),
            ok(none()),
            "Setting a new key-value should return None"
        );
        assert_that!(
            cache.cache_get(&TEST_KEY),
            ok(some(anything())),
            "Getting an existing key-value before it expires should return the value"
        );

        // Let the ttl expire
        sleep(LIFE_SPAN_2_SECS);
        sleep(Duration::from_micros(500)); // a bit extra for good measure
        assert_that!(
            cache.cache_get(&TEST_KEY),
            ok(none()),
            "Getting an expired key-value should return None"
        );
    }

    #[googletest::test]
    fn set_ttl_to_a_different_ttl_is_respected() {
        // COPY PASTE of [values_expire_when_lifespan_elapses_returning_none]
        let tmp_dir = temp_dir!();
        let cache: DiskCache<u32, u32> = DiskCache::new("test-cache")
            .disk_directory(tmp_dir.path())
            .ttl(LIFE_SPAN_2_SECS)
            .build()
            .unwrap();

        assert_that!(
            cache.cache_get(&TEST_KEY),
            ok(none()),
            "Getting a non-existent key-value should return None"
        );

        assert_that!(
            cache.cache_set(TEST_KEY, TEST_VAL),
            ok(none()),
            "Setting a new key-value should return None"
        );

        // Let the ttl expire
        sleep(LIFE_SPAN_2_SECS);
        sleep(Duration::from_micros(500)); // a bit extra for good measure
        assert_that!(
            cache.cache_get(&TEST_KEY),
            ok(none()),
            "Getting an expired key-value should return None"
        );

        let old_from_setting_lifespan =
            ConcurrentCached::set_ttl(&cache, LIFE_SPAN_1_SEC).expect("error setting new ttl");
        assert_that!(
            old_from_setting_lifespan,
            eq(LIFE_SPAN_2_SECS),
            "Setting ttl should return the old ttl"
        );
        assert_that!(
            cache.cache_set(TEST_KEY, TEST_VAL),
            ok(none()),
            "Setting a previously expired key-value should return None"
        );
        assert_that!(
            cache.cache_get(&TEST_KEY),
            ok(some(eq(TEST_VAL))),
            "Getting a newly set (previously expired) key-value should return the value"
        );

        // Let the new ttl expire
        sleep(LIFE_SPAN_1_SEC);
        sleep(Duration::from_micros(500)); // a bit extra for good measure
        assert_that!(
            cache.cache_get(&TEST_KEY),
            ok(none()),
            "Getting an expired key-value should return None"
        );

        ConcurrentCached::set_ttl(&cache, Duration::from_secs(10)).expect("error setting ttl");
        assert_that!(
            cache.cache_set(TEST_KEY, TEST_VAL),
            ok(none()),
            "Setting a previously expired key-value should return None"
        );

        assert_that!(
            cache.cache_get(&TEST_KEY),
            ok(some(eq(TEST_VAL))),
            "Getting a newly set (previously expired) key-value should return the value"
        );
        assert_that!(
            cache.cache_get(&TEST_KEY),
            ok(some(eq(TEST_VAL))),
            "Getting the same value again should return the value"
        );
    }

    #[googletest::test]
    fn refreshing_on_cache_get_delays_cache_expiry() {
        // NOTE: Here we're relying on the fact that setting then sleeping for 2 secs and getting takes longer than 2 secs.
        const LIFE_SPAN: Duration = LIFE_SPAN_2_SECS;
        const HALF_LIFE_SPAN: Duration = LIFE_SPAN_1_SEC;
        let tmp_dir = temp_dir!();
        let cache: DiskCache<u32, u32> = DiskCache::new("test-cache")
            .disk_directory(tmp_dir.path())
            .ttl(LIFE_SPAN)
            .refresh(true) // ENABLE REFRESH - this is what we're testing
            .build()
            .unwrap();

        assert_that!(cache.cache_set(TEST_KEY, TEST_VAL), ok(none()));

        // retrieve before expiry, this should refresh the created_at so we don't expire just yet
        sleep(HALF_LIFE_SPAN);
        assert_that!(
            cache.cache_get(&TEST_KEY),
            ok(some(eq(TEST_VAL))),
            "Getting a value before expiry should return the value"
        );

        // This is after the initial expiry, but since we refreshed the created_at, we should still get the value
        sleep(HALF_LIFE_SPAN);
        assert_that!(
            cache.cache_get(&TEST_KEY),
            ok(some(eq(TEST_VAL))),
            "Getting a value after the initial expiry should return the value as we have refreshed"
        );

        // This is after the new refresh expiry, we should get None
        sleep(LIFE_SPAN);
        assert_that!(
            cache.cache_get(&TEST_KEY),
            ok(none()),
            "Getting a value after the refreshed expiry should return None"
        );

        drop(cache);
    }

    #[googletest::test]
    // Smoke test for the default disk directory: a full get/set/remove
    // round-trip succeeds when `disk_directory` is left at its default.
    fn does_not_break_when_constructed_using_default_disk_directory() {
        let cache: DiskCache<u32, u32> =
            DiskCache::new(&format!("{}:disk-cache-test-default-dir", now_millis()))
                // use the default disk directory
                .build()
                .unwrap();

        let cached = cache.cache_get(&TEST_KEY).unwrap();
        assert_that!(
            cached,
            none(),
            "Getting a non-existent key-value should return None"
        );

        let cached = cache.cache_set(TEST_KEY, TEST_VAL).unwrap();
        assert_that!(cached, none(), "Setting a new key-value should return None");

        let cached = cache.cache_set(TEST_KEY, TEST_VAL_1).unwrap();
        assert_that!(
            cached,
            some(eq(TEST_VAL)),
            "Setting an existing key-value should return the old value"
        );

        // remove the cache dir to clean up the test as we're not using a temp dir
        std::fs::remove_dir_all(cache.disk_path).expect("error in clean up removeing the cache dir")
    }

    mod set_sync_to_disk_on_cache_change {

        mod when_no_auto_flushing {
            use super::super::*;

            fn check_on_recovered_cache(
                set_sync_to_disk_on_cache_change: bool,
                run_on_original_cache: fn(&DiskCache<u32, u32>) -> (),
                run_on_recovered_cache: fn(&DiskCache<u32, u32>) -> (),
            ) {
                let original_cache_tmp_dir = temp_dir!();
                let copied_cache_tmp_dir = temp_dir!(no_exist);
                const CACHE_NAME: &str = "test-cache";

                let cache: DiskCache<u32, u32> = DiskCache::new(CACHE_NAME)
                    .disk_directory(original_cache_tmp_dir.path())
                    .sync_to_disk_on_cache_change(set_sync_to_disk_on_cache_change) // WHAT'S BEING TESTED
                    // NOTE: disabling automatic flushing, so that we only test the flushing of cache_set
                    .connection_config(sled::Config::new().flush_every_ms(None))
                    .build()
                    .unwrap();

                // flush the cache to disk before any cache setting, so that when we create the recovered cache
                // it has something to recover from, even if set_cache doesn't write to disk as we'd like.
                cache
                    .connection
                    .flush()
                    .expect("error flushing cache before any cache setting");

                run_on_original_cache(&cache);

                // freeze the current state of the cache files by copying them to a new location
                // we do this before dropping the cache, as dropping the cache seems to flush to the disk
                let recovered_cache = clone_cache_to_new_location_no_flushing(
                    CACHE_NAME,
                    &cache,
                    copied_cache_tmp_dir.path(),
                );

                assert_that!(recovered_cache.connection.was_recovered(), eq(true));

                run_on_recovered_cache(&recovered_cache);
            }

            mod changes_persist_after_recovery_when_set_to_true {
                use super::*;

                #[googletest::test]
                fn for_cache_set() {
                    check_on_recovered_cache(
                        false,
                        |cache| {
                            // write to the cache, we expect this to persist if the connection is flushed on cache_set
                            cache
                                .cache_set(TEST_KEY, TEST_VAL)
                                .expect("error setting cache in assemble stage");
                        },
                        |recovered_cache| {
                            assert_that!(
                                recovered_cache.cache_get(&TEST_KEY),
                                ok(none()),
                                "set_sync_to_disk_on_cache_change is false, and there is no auto-flushing, so the cache should not have persisted"
                            );
                        },
                    )
                }

                #[googletest::test]
                fn for_cache_remove() {
                    check_on_recovered_cache(
                        false,
                        |cache| {
                            // write to the cache, we expect this to persist if the connection is flushed on cache_set
                            cache
                                .cache_set(TEST_KEY, TEST_VAL)
                                .expect("error setting cache in assemble stage");

                            // manually flush the cache so that we only test cache_remove
                            cache.connection.flush().expect("error flushing cache");

                            cache
                                .cache_remove(&TEST_KEY)
                                .expect("error removing cache in assemble stage");
                        },
                        |recovered_cache| {
                            assert_that!(
                                recovered_cache.cache_get(&TEST_KEY),
                                ok(some(eq(TEST_VAL))),
                                "set_sync_to_disk_on_cache_change is false, and there is no auto-flushing, so the cache_remove should not have persisted"
                            );
                        },
                    )
                }
            }

            /// This is the anti-test
            mod changes_do_not_persist_after_recovery_when_set_to_false {
                use super::*;

                #[googletest::test]
                fn for_cache_set() {
                    check_on_recovered_cache(
                        true,
                        |cache| {
                            // write to the cache, we expect this to persist if the connection is flushed on cache_set
                            cache
                                .cache_set(TEST_KEY, TEST_VAL)
                                .expect("error setting cache in assemble stage");
                        },
                        |recovered_cache| {
                            assert_that!(
                                recovered_cache.cache_get(&TEST_KEY),
                                ok(some(eq(TEST_VAL))),
                                "Getting a set key should return the value"
                            );
                        },
                    )
                }

                #[googletest::test]
                fn for_cache_remove() {
                    check_on_recovered_cache(
                        true,
                        |cache| {
                            // write to the cache, we expect this to persist if the connection is flushed on cache_set
                            cache
                                .cache_set(TEST_KEY, TEST_VAL)
                                .expect("error setting cache in assemble stage");

                            cache
                                .cache_remove(&TEST_KEY)
                                .expect("error removing cache in assemble stage");
                        },
                        |recovered_cache| {
                            assert_that!(
                                recovered_cache.cache_get(&TEST_KEY),
                                ok(none()),
                                "Getting a removed key should return None"
                            );
                        },
                    )
                }
            }

            fn clone_cache_to_new_location_no_flushing(
                cache_name: &str,
                cache: &DiskCache<u32, u32>,
                new_location: &Path,
            ) -> DiskCache<u32, u32> {
                copy_dir::copy_dir(cache.disk_path.parent().unwrap(), new_location)
                    .expect("error copying cache files to new location");

                DiskCache::new(cache_name)
                    .disk_directory(new_location)
                    .build()
                    .expect("error building cache from copied files")
            }
        }
    }
}
