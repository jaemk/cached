use crate::time::Duration;
use crate::time::SystemTime;
use crate::{ConcurrentCacheBase, ConcurrentCacheTtl, ConcurrentCached};
use directories::BaseDirs;
use parking_lot::Mutex;
use redb::{Builder, Database, Durability, ReadableDatabase, ReadableTable, TableDefinition};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::io::ErrorKind;
use std::marker::PhantomData;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// The single redb table used for all disk cache entries. Keys are the
/// stringified cache keys, values are the rmp-serialized [`CachedDiskValue`].
const TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("cached_disk_cache");

pub struct RedbCacheBuilder<K, V> {
    ttl: Option<Duration>,
    refresh: bool,
    durable: bool,
    disk_dir: Option<PathBuf>,
    cache_name: Option<String>,
    // fn-pointer phantom — see the rationale on `RedbCache::_phantom`; keeps the
    // type unconditionally `Send + Sync` regardless of `K`/`V`.
    _phantom: PhantomData<fn() -> (K, V)>,
}

use thiserror::Error;

/// Convert redb's per-operation error types into `$t` by routing through
/// [`redb::Error`] (for which `$t` already has a `#[from]`).
macro_rules! impl_from_redb {
    ($t:ty; $($s:ty),+ $(,)?) => {
        $(
            impl From<$s> for $t {
                fn from(e: $s) -> Self {
                    <$t>::from(redb::Error::from(e))
                }
            }
        )+
    };
}

/// Error returned when building a [`RedbCache`].
///
/// Configuration problems (a missing `name`, or a zero `ttl`) surface as the transparent
/// [`Build`](Self::Build) variant wrapping a [`BuildError`](super::BuildError):
///
/// ```ignore
/// match RedbCache::<String, u32>::builder().build() {
///     Err(RedbCacheBuildError::Build(BuildError::MissingRequired(field))) => { /* e.g. "name" */ }
///     Err(RedbCacheBuildError::Build(BuildError::InvalidValue { field, reason })) => { /* e.g. "ttl" */ }
///     _ => {}
/// }
/// ```
#[non_exhaustive]
#[derive(Error, Debug)]
pub enum RedbCacheBuildError {
    #[error("Storage error")]
    Storage {
        #[from]
        source: redb::Error,
    },
    #[error(transparent)]
    Build(#[from] super::BuildError),
    #[error("I/O error preparing the disk cache directory")]
    Io(#[from] std::io::Error),
    /// The `cache_name` passed to [`RedbCacheBuilder`] is invalid: it must not be empty,
    /// must not contain a path separator (`/` or `\`), must not contain a NUL byte (`\0`),
    /// and must not be `.` or `..`.
    /// These characters would allow the name to escape the cache directory, embed a NUL
    /// in the filename, or produce a meaningless filename when used as a filename component.
    #[error(
        "invalid cache_name: must not be empty, must not contain a path separator ('/' or '\\\\'), \
        must not contain a NUL byte, and must not be '.' or '..'; cache_name is used as a filename component"
    )]
    InvalidCacheName,
}

impl_from_redb!(
    RedbCacheBuildError;
    redb::DatabaseError,
    redb::TransactionError,
    redb::TableError,
    redb::CommitError,
);

static DISK_FILE_PREFIX: &str = "cached_disk_cache";
// Bumped whenever the on-disk format changes (the redb migration, then dropping the
// per-entry `version` field), so an incompatible older file is never read.
const DISK_FILE_VERSION: u64 = 3;

impl<K, V> Default for RedbCacheBuilder<K, V>
where
    K: ToString,
    V: Serialize + DeserializeOwned,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<K, V> RedbCacheBuilder<K, V>
where
    K: ToString,
    V: Serialize + DeserializeOwned,
{
    /// Initialize a `RedbCacheBuilder`.
    ///
    /// The cache name is required; set it with [`name`](Self::name) before calling
    /// [`build`](Self::build).
    #[must_use]
    pub fn new() -> RedbCacheBuilder<K, V> {
        Self {
            ttl: None,
            refresh: false,
            durable: true,
            disk_dir: None,
            cache_name: None,
            _phantom: Default::default(),
        }
    }

    /// Set the cache name (required). Used as a filename component for the on-disk
    /// database file, so it must not be empty, contain a path separator (`/` or `\`),
    /// contain a NUL byte, or be `.` or `..`.
    #[must_use]
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.cache_name = Some(name.into());
        self
    }

    /// Specify the cache TTL as a `Duration`.
    ///
    /// **TTL is optional.** When no TTL is set (the default), entries never
    /// expire and are kept until explicitly removed or the cache is cleared.
    /// This is the primary difference from [`RedisCache`](crate::stores::RedisCache),
    /// where a TTL is required.
    ///
    /// Overrides any previously set ttl/ttl_secs/ttl_millis on this builder.
    #[must_use]
    pub fn ttl(mut self, ttl: Duration) -> Self {
        self.ttl = Some(ttl);
        self
    }

    /// Specify the cache TTL in whole seconds. Equivalent to
    /// `ttl(Duration::from_secs(secs))`.
    ///
    /// Overrides any previously set ttl/ttl_secs/ttl_millis on this builder.
    #[must_use]
    pub fn ttl_secs(self, secs: u64) -> Self {
        self.ttl(Duration::from_secs(secs))
    }

    /// Specify the cache TTL in milliseconds. Equivalent to
    /// `ttl(Duration::from_millis(millis))`.
    ///
    /// Overrides any previously set ttl/ttl_secs/ttl_millis on this builder.
    #[must_use]
    pub fn ttl_millis(self, millis: u64) -> Self {
        self.ttl(Duration::from_millis(millis))
    }

    /// Specify whether cache hits refresh the TTL
    #[must_use]
    pub fn refresh_on_hit(mut self, refresh: bool) -> Self {
        self.refresh = refresh;
        self
    }

    /// Set the disk path for where the data will be stored
    #[must_use]
    pub fn disk_directory<P: AsRef<Path>>(mut self, dir: P) -> Self {
        self.disk_dir = Some(dir.as_ref().into());
        self
    }

    /// Set whether writes are durable: fsync'd to disk before the call returns.
    ///
    /// When `true` (the default), every write commits with
    /// [`redb::Durability::Immediate`], guaranteeing the change is fsync'd to disk
    /// before the call returns. This makes the cache durable by default, which is
    /// usually what you want from a disk-backed store.
    ///
    /// Set `false` to trade durability for write throughput: writes then commit with
    /// [`redb::Durability::None`] (no fsync). Per redb's semantics, a `Durability::None`
    /// commit is not guaranteed to be persisted until a later `Durability::Immediate`
    /// commit occurs, so writes made with `false` may be lost on process exit or crash,
    /// not only on an unclean shutdown. When using `false`, call [`RedbCache::flush`]
    /// (or [`RedbCache::async_flush`]) at chosen points to force a durable commit.
    #[must_use]
    pub fn durable(mut self, durable: bool) -> Self {
        self.durable = durable;
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

    /// Find (and create) a writable default directory in which to place the
    /// redb database file, returning the directory path.
    fn default_disk_path() -> Result<PathBuf, std::io::Error> {
        let candidates = Self::default_disk_dir_candidates();
        // The last candidate is always the temp_dir fallback. All earlier
        // candidates use the user's XDG cache dir and are treated as preferred.
        let last_idx = candidates.len().saturating_sub(1);
        let mut last_error = None;

        for (idx, disk_dir) in candidates.into_iter().enumerate() {
            let is_temp_fallback = idx == last_idx;
            match create_cache_dir(&disk_dir) {
                Ok(()) => {
                    // On unix, when using the temp_dir fallback, validate the
                    // resolved path to guard against symlink-based TOCTOU
                    // attacks: reject symlinks and world/group-writable dirs.
                    #[cfg(unix)]
                    if is_temp_fallback {
                        validate_temp_cache_dir(&disk_dir)?;
                    }
                    #[cfg(not(unix))]
                    let _ = is_temp_fallback;
                    return Ok(disk_dir);
                }
                Err(error) if error.kind() == ErrorKind::PermissionDenied => {
                    last_error = Some(error);
                }
                Err(error) => return Err(error),
            }
        }

        Err(last_error.unwrap_or_else(|| {
            std::io::Error::new(
                ErrorKind::PermissionDenied,
                "unable to create a writable default disk cache directory",
            )
        }))
    }

    /// Build the `RedbCache`, validating configuration and opening (or creating)
    /// the on-disk redb database file.
    ///
    /// # Errors
    ///
    /// - `Build(BuildError::MissingRequired("name"))`: no cache name was set.
    /// - `InvalidCacheName`: `cache_name` is empty, contains a path separator
    ///   (`/` or `\`), contains a NUL byte, or is the path-traversal component `.` or `..`.
    /// - `Build(BuildError::InvalidValue { field: "ttl", .. })`: the configured TTL is zero.
    /// - `Io`: the cache directory could not be created.
    /// - `Storage`: the redb database file could not be opened or initialized.
    pub fn build(self) -> Result<RedbCache<K, V>, RedbCacheBuildError> {
        let cache_name = self
            .cache_name
            .ok_or(super::BuildError::MissingRequired("name"))?;
        // Validate cache_name before using it as a filename component.
        // An empty name yields a meaningless filename. A name containing a path
        // separator ('/' or '\\') or a NUL byte can silently escape the cache
        // directory or create nested subdirectories; those are the checks that
        // actually prevent traversal. The '.' and '..' checks are
        // belt-and-suspenders: because the name is always suffixed with
        // `_v<VERSION>.redb`, a bare '.' or '..' can never reach the filesystem
        // as a traversal component, but they are rejected anyway as nonsensical
        // names. (':' is allowed: it is established usage in
        // module-path-derived names.)
        {
            let n = &cache_name;
            if n.is_empty()
                || n.contains('/')
                || n.contains('\\')
                || n.contains('\0')
                || n == "."
                || n == ".."
            {
                return Err(RedbCacheBuildError::InvalidCacheName);
            }
        }
        if let Some(ttl) = self.ttl {
            super::validate_ttl(ttl)?;
        }
        let cache_dir_name = format!("{}_v{}", cache_name, DISK_FILE_VERSION);

        // redb stores a single file. Resolve the directory (explicit or
        // default), ensure it exists, then use `<cache_dir_name>.redb` inside it
        // as the database file.
        let disk_dir = match self.disk_dir {
            Some(disk_dir) => {
                create_cache_dir(&disk_dir)?;
                disk_dir
            }
            None => Self::default_disk_path()?,
        };
        let disk_path = disk_dir.join(format!("{}.redb", cache_dir_name));

        // On unix, pre-create the redb file with mode 0600 so that the
        // database bytes are never readable by group or other. We use
        // OpenOptions to create (or open) the file with the correct mode
        // before redb opens it; redb will then open the existing file.
        // On non-unix platforms we skip this step and let redb create the file
        // with default OS permissions.
        #[cfg(unix)]
        {
            use std::fs::OpenOptions;
            use std::os::unix::fs::OpenOptionsExt;
            OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .open(&disk_path)?;
        }

        let db = Builder::new().create(&disk_path)?;

        // Create the table once at build time so that read transactions always
        // find it (a read txn `open_table` on a never-created table errors with
        // `TableError::TableDoesNotExist`).
        {
            let wtxn = db.begin_write()?;
            wtxn.open_table(TABLE)?;
            wtxn.commit()?;
        }

        Ok(RedbCache {
            ttl: Mutex::new(self.ttl),
            refresh: AtomicBool::new(self.refresh),
            durable: self.durable,
            disk_path,
            connection: Arc::new(db),
            _phantom: self._phantom,
        })
    }
}

/// Create a directory (and all parents) for storing the redb database file.
///
/// On unix the directory is created with mode 0700 (owner read/write/execute
/// only) so that the database file is not visible to other users. On non-unix
/// platforms `std::fs::create_dir_all` is used and the OS decides the mode.
fn create_cache_dir(path: &Path) -> Result<(), std::io::Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(path)
    }
}

/// On unix, validate that the resolved cache directory path is not a symlink
/// and is not group- or world-writable. This guards the `temp_dir` fallback
/// path against symlink-based TOCTOU attacks where an adversary replaces the
/// target directory with a symlink pointing elsewhere.
///
/// Uid ownership is not checked here because obtaining the process uid without
/// a dependency (e.g. `libc` or `rustix`) would require unsafe platform calls.
/// The symlink + permission-bits check is sufficient to reject the most
/// common attack vectors (world-writable directory, symlink redirection).
#[cfg(unix)]
fn validate_temp_cache_dir(path: &Path) -> Result<(), std::io::Error> {
    use std::os::unix::fs::MetadataExt;

    let meta = std::fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        return Err(std::io::Error::new(
            ErrorKind::PermissionDenied,
            "temp cache directory is a symlink; refusing to use it",
        ));
    }
    // Reject group-writable (0o020) or other-writable (0o002) directories.
    let mode = meta.mode();
    if mode & 0o022 != 0 {
        return Err(std::io::Error::new(
            ErrorKind::PermissionDenied,
            "temp cache directory has insecure permissions (group- or world-writable)",
        ));
    }
    Ok(())
}

/// Cache store backed by disk, using an embedded [`redb`](https://crates.io/crates/redb)
/// database (one file per cache).
///
/// # TTL
///
/// TTL is **optional**. When no TTL is configured (the default), entries never expire and
/// persist until explicitly removed or the cache is cleared. This differs from
/// [`RedisCache`](crate::stores::RedisCache), where a TTL is required. Set a TTL via
/// [`RedbCacheBuilder::ttl`] / [`RedbCacheBuilder::ttl_secs`] / [`RedbCacheBuilder::ttl_millis`]
/// at build time, or update it at runtime with [`ConcurrentCacheTtl::set_ttl`].
///
/// # Concurrency and performance
///
/// redb is a single-writer store. Each `cache_set` / `cache_remove` / `cache_clear` runs
/// in its own write transaction, and write transactions on one `RedbCache` are serialized
/// (only one commits at a time). Reads are MVCC: they run concurrently with each other and
/// with a writer, so they never block. The async operations run the blocking redb work on a
/// background thread (via [`blocking::unblock`]), so concurrent async writers also queue
/// behind the single writer.
///
/// This suits read-heavy caching. If a single `RedbCache` is written from many threads at
/// once, write throughput is bounded by the serialized writer. To reduce that cost, spread
/// the load across multiple `RedbCache` instances, each with a distinct cache name (redb
/// takes an exclusive lock on its file, so two instances sharing one name/path cannot be
/// opened at once), and/or set
/// [`durable`](RedbCacheBuilder::durable)
/// `false` so commits skip the fsync (trading durability for speed).
pub struct RedbCache<K, V> {
    pub(super) ttl: Mutex<Option<Duration>>,
    pub(super) refresh: AtomicBool,
    durable: bool,
    disk_path: PathBuf,
    connection: Arc<Database>,
    // `RedbCache`/`RedbCacheBuilder` own no live `K`/`V` (values are serialized
    // to disk; `K`/`V` only appear in method signatures). Use a fn-pointer
    // phantom so the type is unconditionally `Send + Sync` and does not impose
    // `K: Sync`/`V: Sync` on callers (e.g. the async impl). Variance is
    // unchanged: covariant in `K` and `V`, same as `PhantomData<(K, V)>`.
    _phantom: PhantomData<fn() -> (K, V)>,
}

impl<K, V> std::fmt::Debug for RedbCache<K, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedbCache")
            .field("disk_path", &self.disk_path)
            .field("ttl", &*self.ttl.lock())
            .field("refresh", &self.refresh.load(Ordering::Relaxed))
            .field("durable", &self.durable)
            .finish_non_exhaustive()
    }
}

impl<K, V> RedbCache<K, V>
where
    K: ToString,
    V: Serialize + DeserializeOwned,
{
    /// Initialize a `RedbCacheBuilder`.
    ///
    /// The cache name is required; set it via [`RedbCacheBuilder::name`] before
    /// calling [`build`](RedbCacheBuilder::build). If it is missing, `build` returns
    /// `Err(`[`BuildError::MissingRequired`](super::BuildError::MissingRequired)`)` rather
    /// than panicking.
    #[must_use]
    pub fn builder() -> RedbCacheBuilder<K, V> {
        RedbCacheBuilder::new()
    }

    /// Return the path of the on-disk redb database file backing this cache.
    #[must_use]
    pub fn disk_path(&self) -> &std::path::Path {
        &self.disk_path
    }

    /// Remove all entries whose TTL has elapsed, returning the number of entries
    /// removed (aligning with [`CacheEvict::evict`](crate::CacheEvict::evict) /
    /// [`ConcurrentCacheEvict::evict`](crate::ConcurrentCacheEvict::evict), which
    /// also return `usize`).
    pub fn remove_expired_entries(&self) -> Result<usize, RedbCacheError> {
        let now = SystemTime::now();
        let ttl = *self.ttl.lock();

        // Collect the keys to expire first: we cannot remove entries while
        // holding the read iterator (the iterator borrows the read txn).
        let mut expired_keys: Vec<String> = Vec::new();
        {
            let rtxn = self.connection.begin_read()?;
            let table = rtxn.open_table(TABLE)?;
            for item in table.iter()? {
                let (key, value) = item?;
                let raw = value.value();
                let cached =
                    rmp_serde::from_slice::<CachedDiskValue<V>>(raw).map_err(|source| {
                        RedbCacheError::CacheDeserialization {
                            source,
                            cached_value: raw.to_vec(),
                        }
                    })?;
                if let Some(ttl) = ttl
                    && now
                        .duration_since(cached.created_at)
                        .unwrap_or(Duration::from_secs(0))
                        >= ttl
                {
                    expired_keys.push(key.value().to_string());
                }
            }
        }

        if !expired_keys.is_empty() {
            let wtxn = begin_write(&self.connection, self.durable)?;
            {
                let mut table = wtxn.open_table(TABLE)?;
                for key in &expired_keys {
                    table.remove(key.as_str())?;
                }
            }
            wtxn.commit()?;
        }

        Ok(expired_keys.len())
    }

    /// Force a durable (fsync) commit, persisting any writes made while
    /// [`durable`](RedbCacheBuilder::durable)
    /// is `false`.
    ///
    /// With `durable(false)` writes commit with `Durability::None`:
    /// they are fast but are not guaranteed to survive a process exit or crash until a
    /// later durable commit. Call `flush()` periodically or before shutdown to get
    /// explicit durability points while keeping cheap writes the rest of the time. It
    /// commits an empty transaction with immediate durability, so it is safe to call at
    /// any time (including on an empty cache); when `durable` is
    /// `true` (the default) every write is already durable and this is effectively a no-op.
    pub fn flush(&self) -> Result<(), RedbCacheError> {
        redb_flush(&self.connection)
    }
}

/// Async counterpart of [`RedbCache::flush`].
#[cfg(feature = "async")]
#[cfg_attr(docsrs, doc(cfg(feature = "async")))]
impl<K, V> RedbCache<K, V> {
    /// Async counterpart of [`flush`](RedbCache::flush): runs the durable (fsync)
    /// commit on a background thread (via the [`blocking`] crate) so it does not
    /// stall the async runtime.
    pub async fn async_flush(&self) -> Result<(), RedbCacheError> {
        let connection = self.connection.clone();
        blocking::unblock(move || redb_flush(&connection)).await
    }
}

#[non_exhaustive]
#[derive(Error, Debug)]
pub enum RedbCacheError {
    #[error("Storage error")]
    Storage {
        #[from]
        source: redb::Error,
    },
    /// A stored value failed to deserialize.
    ///
    /// **Security note:** `cached_value` contains the raw bytes that were read
    /// from disk and failed to decode. Those bytes may contain sensitive
    /// application data. Do not log or display this error variant without
    /// redacting or omitting the `cached_value` field.
    #[error("Error deserializing cached value")]
    CacheDeserialization {
        #[source]
        source: rmp_serde::decode::Error,
        cached_value: Vec<u8>,
    },
    #[error("Error serializing cached value")]
    CacheSerialization {
        #[from]
        source: rmp_serde::encode::Error,
    },
}

impl_from_redb!(
    RedbCacheError;
    redb::TransactionError,
    redb::TableError,
    redb::StorageError,
    redb::CommitError,
    redb::SetDurabilityError,
);

#[derive(serde::Serialize, serde::Deserialize)]
struct CachedDiskValue<V> {
    value: V,
    created_at: SystemTime,
}

impl<V> CachedDiskValue<V> {
    fn new(value: V) -> Self {
        Self {
            value,
            created_at: SystemTime::now(),
        }
    }

    fn refresh_created_at(&mut self) {
        self.created_at = SystemTime::now();
    }
}

/// Borrowed counterpart of [`CachedDiskValue`] used by `cache_set_ref` to
/// serialize from a `&V` without cloning. It serializes to the same bytes as
/// `CachedDiskValue::new(value)` (same field names and order), so values written
/// through either path deserialize identically.
#[derive(serde::Serialize)]
struct CachedDiskValueRef<'a, V> {
    value: &'a V,
    created_at: SystemTime,
}

impl<'a, V> CachedDiskValueRef<'a, V> {
    fn new(value: &'a V) -> Self {
        Self {
            value,
            created_at: SystemTime::now(),
        }
    }
}

// ── Connection-level disk operations ─────────────────────────────────────────
//
// These free functions hold the single source of truth for the on-disk
// behavior (TTL/refresh handling, serialization-error propagation, durability).
// The synchronous `ConcurrentCached` impl calls them directly; the async
// `ConcurrentCachedAsync` impl calls them inside `blocking::unblock` so
// the blocking `redb` I/O does not stall the async runtime. Keeping one
// implementation guarantees the sync and async paths stay behaviorally
// identical.
//
// `durable` maps to redb's durability: `true` uses the
// default durable (`Durability::Immediate`) commit, `false` uses
// `Durability::None` (deferred fsync). This is applied to every write txn.

/// Begin a write txn with the durability implied by `durable`.
/// Durability is set on the fresh transaction (it only needs to be set before the
/// eventual `commit`); callers then open the table and commit.
fn begin_write(
    connection: &Database,
    durable: bool,
) -> Result<redb::WriteTransaction, RedbCacheError> {
    let mut wtxn = connection.begin_write()?;
    if !durable {
        wtxn.set_durability(Durability::None)?;
    }
    Ok(wtxn)
}

fn disk_cache_get<V>(
    connection: &Database,
    key: &str,
    ttl: Option<Duration>,
    refresh: bool,
    durable: bool,
) -> Result<Option<V>, RedbCacheError>
where
    V: Serialize + DeserializeOwned,
{
    let mut cached = {
        let rtxn = connection.begin_read()?;
        let table = rtxn.open_table(TABLE)?;
        let Some(guard) = table.get(key)? else {
            return Ok(None);
        };
        // Deserialize before the guard/table/txn are dropped.
        let raw = guard.value();
        rmp_serde::from_slice::<CachedDiskValue<V>>(raw).map_err(|source| {
            RedbCacheError::CacheDeserialization {
                source,
                cached_value: raw.to_vec(),
            }
        })?
    };

    if let Some(ttl) = ttl {
        if SystemTime::now()
            .duration_since(cached.created_at)
            .unwrap_or(Duration::from_secs(0))
            < ttl
        {
            if refresh {
                cached.refresh_created_at();
                let serialized = rmp_serde::to_vec(&cached)?;
                let wtxn = begin_write(connection, durable)?;
                {
                    let mut table = wtxn.open_table(TABLE)?;
                    table.insert(key, serialized.as_slice())?;
                }
                wtxn.commit()?;
            }
            Ok(Some(cached.value))
        } else {
            let wtxn = begin_write(connection, durable)?;
            {
                let mut table = wtxn.open_table(TABLE)?;
                table.remove(key)?;
            }
            wtxn.commit()?;
            Ok(None)
        }
    } else {
        Ok(Some(cached.value))
    }
}

fn disk_cache_set<V>(
    connection: &Database,
    key: &str,
    serialized: Vec<u8>,
    durable: bool,
) -> Result<Option<V>, RedbCacheError>
where
    V: DeserializeOwned,
{
    let wtxn = begin_write(connection, durable)?;
    // Copy the previous value's bytes (owned) before the guard/table are dropped,
    // but defer deserialization until after the commit: the new value must be
    // written regardless of whether the displaced value can be decoded. The set
    // itself succeeded, so an undecodable previous value is reported as `None`
    // (there is no recoverable previous value) rather than surfaced as an error.
    let previous_bytes: Option<Vec<u8>> = {
        let mut table = wtxn.open_table(TABLE)?;
        table
            .insert(key, serialized.as_slice())?
            .map(|guard| guard.value().to_vec())
    };
    wtxn.commit()?;
    Ok(previous_bytes
        .and_then(|bytes| rmp_serde::from_slice::<CachedDiskValue<V>>(&bytes).ok())
        .map(|cached| cached.value))
}

fn disk_cache_remove<V>(
    connection: &Database,
    key: &str,
    ttl: Option<Duration>,
    durable: bool,
) -> Result<Option<V>, RedbCacheError>
where
    V: DeserializeOwned,
{
    let wtxn = begin_write(connection, durable)?;
    // Copy the removed bytes (owned) and commit before deserializing, so the entry
    // is removed regardless of whether its value can be decoded. The removal
    // succeeded, so an undecodable value is reported as `None` rather than an error.
    let removed_bytes: Option<Vec<u8>> = {
        let mut table = wtxn.open_table(TABLE)?;
        table.remove(key)?.map(|guard| guard.value().to_vec())
    };
    wtxn.commit()?;

    let removed =
        removed_bytes.and_then(|bytes| rmp_serde::from_slice::<CachedDiskValue<V>>(&bytes).ok());
    let result = if let Some(cached) = removed {
        if let Some(ttl) = ttl {
            if SystemTime::now()
                .duration_since(cached.created_at)
                .unwrap_or(Duration::from_secs(0))
                < ttl
            {
                Some(cached.value)
            } else {
                None
            }
        } else {
            Some(cached.value)
        }
    } else {
        None
    };

    Ok(result)
}

fn disk_cache_remove_entry<V>(
    connection: &Database,
    key: &str,
    durable: bool,
) -> Result<Option<V>, RedbCacheError>
where
    V: DeserializeOwned,
{
    let wtxn = begin_write(connection, durable)?;
    // Copy the removed bytes (owned) and commit before deserializing, so the entry
    // is removed regardless of whether its value can be decoded. The removal
    // succeeded, so an undecodable value is reported as `None` rather than an error.
    let removed_bytes: Option<Vec<u8>> = {
        let mut table = wtxn.open_table(TABLE)?;
        table.remove(key)?.map(|guard| guard.value().to_vec())
    };
    wtxn.commit()?;
    Ok(removed_bytes
        .and_then(|bytes| rmp_serde::from_slice::<CachedDiskValue<V>>(&bytes).ok())
        .map(|cached| cached.value))
}

fn disk_cache_delete(
    connection: &Database,
    key: &str,
    durable: bool,
) -> Result<bool, RedbCacheError> {
    let wtxn = begin_write(connection, durable)?;
    let removed = {
        let mut table = wtxn.open_table(TABLE)?;
        table.remove(key)?.is_some()
    };
    wtxn.commit()?;
    Ok(removed)
}

/// Remove every entry from the cache table. Drops and recreates the table in a
/// single write txn so subsequent read txns still find an (empty) table rather
/// than erroring with `TableError::TableDoesNotExist`.
fn disk_cache_clear(connection: &Database, durable: bool) -> Result<(), RedbCacheError> {
    let wtxn = begin_write(connection, durable)?;
    wtxn.delete_table(TABLE)?;
    wtxn.open_table(TABLE)?;
    wtxn.commit()?;
    Ok(())
}

/// Force a durable (fsync) commit. An empty write transaction committed with
/// [`Durability::Immediate`] persists everything written so far, including prior
/// `Durability::None` commits (the writes made while `durable`
/// is `false`).
fn redb_flush(connection: &Database) -> Result<(), RedbCacheError> {
    let mut wtxn = connection.begin_write()?;
    wtxn.set_durability(Durability::Immediate)?;
    wtxn.commit()?;
    Ok(())
}

/// Behavior on a corrupt stored value (one whose bytes fail to deserialize):
/// `cache_get` and `remove_expired_entries` surface a
/// [`RedbCacheError::CacheDeserialization`]. `cache_set`, `cache_remove`, and
/// `cache_remove_entry` instead succeed — they write/remove the entry regardless and
/// report the undecodable previous value as `Ok(None)` (a write that took effect is
/// never reported as an error). The same holds for the `ConcurrentCachedAsync` impl.
///
/// `cache_get` can additionally surface a [`RedbCacheError::CacheSerialization`] when
/// `refresh_on_hit` is enabled and re-serializing the just-read entry to rewrite its
/// refreshed expiry fails.
impl<K, V> ConcurrentCacheBase for RedbCache<K, V> {
    type Error = RedbCacheError;
}

impl<K, V> ConcurrentCacheTtl for RedbCache<K, V> {
    fn ttl(&self) -> Option<Duration> {
        *self.ttl.lock()
    }

    /// Set the TTL applied to newly inserted entries, returning the previous TTL
    /// (`None` if expiry was disabled).
    ///
    /// A zero `ttl` disables expiry, exactly equivalent to `unset_ttl`: subsequent writes
    /// store entries with no expiry. Existing entries keep the expiry they were written with.
    fn set_ttl(&self, ttl: Duration) -> Option<Duration> {
        let mut guard = self.ttl.lock();
        if ttl.is_zero() {
            guard.take()
        } else {
            guard.replace(ttl)
        }
    }

    fn unset_ttl(&self) -> Option<Duration> {
        self.ttl.lock().take()
    }

    fn refresh_on_hit(&self) -> bool {
        self.refresh.load(Ordering::Relaxed)
    }

    fn set_refresh_on_hit(&self, refresh: bool) -> bool {
        self.refresh.swap(refresh, Ordering::Relaxed)
    }
}

impl<K, V> ConcurrentCached<K, V> for RedbCache<K, V>
where
    K: ToString + Clone,
    V: Serialize + DeserializeOwned,
{
    fn cache_get(&self, key: &K) -> Result<Option<V>, RedbCacheError> {
        let ttl = *self.ttl.lock();
        let refresh = self.refresh.load(Ordering::Relaxed);
        disk_cache_get(
            &self.connection,
            &key.to_string(),
            ttl,
            refresh,
            self.durable,
        )
    }

    fn cache_set(&self, key: K, value: V) -> Result<Option<V>, RedbCacheError> {
        let serialized = rmp_serde::to_vec(&CachedDiskValue::new(value))?;
        disk_cache_set(&self.connection, &key.to_string(), serialized, self.durable)
    }

    fn cache_remove(&self, key: &K) -> Result<Option<V>, RedbCacheError> {
        let ttl = *self.ttl.lock();
        disk_cache_remove(&self.connection, &key.to_string(), ttl, self.durable)
    }

    fn cache_remove_entry(&self, key: &K) -> Result<Option<(K, V)>, Self::Error> {
        disk_cache_remove_entry(&self.connection, &key.to_string(), self.durable)
            .map(|opt| opt.map(|v| (key.clone(), v)))
    }

    fn cache_delete(&self, key: &K) -> Result<bool, RedbCacheError> {
        disk_cache_delete(&self.connection, &key.to_string(), self.durable)
    }

    /// Clear the on-disk cache table, removing every entry.
    ///
    /// Unlike the [`ConcurrentCached::cache_clear`] default (a no-op for
    /// external stores), `RedbCache` overrides this to actually empty its
    /// backing redb table: clearing a local file is cheap and expected.
    /// Durability of the clear follows `durable` (same as
    /// every other write).
    fn cache_clear(&self) -> Result<(), RedbCacheError> {
        disk_cache_clear(&self.connection, self.durable)
    }

    /// Reset the on-disk cache table. `RedbCache` tracks no in-memory metrics,
    /// so this is identical to [`cache_clear`](RedbCache::cache_clear): it
    /// empties the backing redb table (durability per
    /// `durable`).
    fn cache_reset(&self) -> Result<(), RedbCacheError> {
        disk_cache_clear(&self.connection, self.durable)
    }
}

impl<K, V> crate::SerializeCached<K, V> for RedbCache<K, V>
where
    K: ToString + Clone,
    V: Serialize + DeserializeOwned,
{
    /// Serializes from the borrowed `value` (no clone) and writes it under
    /// `key.to_string()`, returning the previous value if any. Equivalent to
    /// [`ConcurrentCached::cache_set`] but avoids taking ownership of `value`.
    fn cache_set_ref(&self, key: &K, value: &V) -> Result<Option<V>, RedbCacheError> {
        let serialized = rmp_serde::to_vec(&CachedDiskValueRef::new(value))?;
        disk_cache_set(&self.connection, &key.to_string(), serialized, self.durable)
    }
}

/// Async disk cache. `redb` has no async API, so every operation is run on
/// a background thread via [`blocking::unblock`] to avoid stalling the async
/// runtime. This is runtime-agnostic: it works with any async executor (tokio,
/// async-std, smol, etc.). Behavior is identical to the synchronous
/// [`ConcurrentCached`] impl (they share the `disk_cache_*` helpers).
///
/// Values need only be `Send`, **not `Sync`**: they are serialized before the
/// work moves onto the blocking pool, so no `V` is held across the `.await`
/// (only the owned serialized bytes).
/// Keys keep `Send + Sync` (the `&K` is borrowed across the await), consistent
/// with the `RedisCache`/`AsyncRedisCache` async stores.
///
/// Cancellation: dropping the returned future does **not** cancel the in-flight
/// blocking `redb` operation — it runs to completion on the background thread
/// (only the result is discarded). This is safe for a cache (`redb`
/// transactions are atomic, so no corruption), but a cancelled `cache_set`/
/// `cache_remove` may still have taken effect on disk.
#[cfg(feature = "async")]
#[cfg_attr(docsrs, doc(cfg(feature = "async")))]
impl<K, V> crate::ConcurrentCachedAsync<K, V> for RedbCache<K, V>
where
    K: ToString + Clone + Send + Sync,
    V: Serialize + DeserializeOwned + Send + 'static,
{
    async fn async_cache_get(&self, key: &K) -> Result<Option<V>, RedbCacheError> {
        let connection = self.connection.clone();
        let key = key.to_string();
        let (ttl, refresh, durable) = (
            *self.ttl.lock(),
            self.refresh.load(Ordering::Relaxed),
            self.durable,
        );
        blocking::unblock(move || disk_cache_get::<V>(&connection, &key, ttl, refresh, durable))
            .await
    }

    async fn async_cache_set(&self, key: K, value: V) -> Result<Option<V>, RedbCacheError> {
        let connection = self.connection.clone();
        let key = key.to_string();
        let durable = self.durable;
        let serialized = rmp_serde::to_vec(&CachedDiskValue::new(value))?;
        blocking::unblock(move || disk_cache_set::<V>(&connection, &key, serialized, durable)).await
    }

    async fn async_cache_remove(&self, key: &K) -> Result<Option<V>, RedbCacheError> {
        let connection = self.connection.clone();
        let key = key.to_string();
        let (ttl, durable) = (*self.ttl.lock(), self.durable);
        blocking::unblock(move || disk_cache_remove::<V>(&connection, &key, ttl, durable)).await
    }

    async fn async_cache_remove_entry(&self, key: &K) -> Result<Option<(K, V)>, Self::Error> {
        let connection = self.connection.clone();
        let key_str = key.to_string();
        let durable = self.durable;
        let v: Option<V> =
            blocking::unblock(move || disk_cache_remove_entry::<V>(&connection, &key_str, durable))
                .await?;
        Ok(v.map(|v| (key.clone(), v)))
    }

    async fn async_cache_delete(&self, key: &K) -> Result<bool, RedbCacheError> {
        let connection = self.connection.clone();
        let key = key.to_string();
        let durable = self.durable;
        blocking::unblock(move || disk_cache_delete(&connection, &key, durable)).await
    }

    /// Async counterpart of [`ConcurrentCached::cache_clear`]: clears the
    /// on-disk table off the async runtime via a background thread (durability
    /// per `durable`).
    async fn async_cache_clear(&self) -> Result<(), RedbCacheError> {
        let connection = self.connection.clone();
        let durable = self.durable;
        blocking::unblock(move || disk_cache_clear(&connection, durable)).await
    }

    /// Async counterpart of [`ConcurrentCached::cache_reset`]. `RedbCache`
    /// tracks no in-memory metrics, so this is identical to
    /// [`ConcurrentCachedAsync::async_cache_clear`](crate::ConcurrentCachedAsync::async_cache_clear).
    async fn async_cache_reset(&self) -> Result<(), RedbCacheError> {
        let connection = self.connection.clone();
        let durable = self.durable;
        blocking::unblock(move || disk_cache_clear(&connection, durable)).await
    }
}

#[cfg(feature = "async")]
#[cfg_attr(docsrs, doc(cfg(feature = "async")))]
impl<K, V> crate::SerializeCachedAsync<K, V> for RedbCache<K, V>
where
    K: ToString + Clone + Send + Sync,
    V: Serialize + DeserializeOwned + Send + 'static,
{
    /// Serializes from the borrowed `value` (no clone) before moving the bytes
    /// onto the background thread. Async counterpart of
    /// [`SerializeCached::cache_set_ref`](crate::SerializeCached::cache_set_ref).
    ///
    /// Serialization happens eagerly (before the returned future is awaited) so the
    /// borrowed `&V` is never held across the `.await`. This keeps the `V: Send`
    /// (not `Sync`) bound consistent with `async_cache_set`.
    fn async_cache_set_ref(
        &self,
        key: &K,
        value: &V,
    ) -> impl std::future::Future<Output = Result<Option<V>, RedbCacheError>> + Send {
        let connection = self.connection.clone();
        let key = key.to_string();
        let durable = self.durable;
        // Serialize eagerly; defer any error into the future.
        let serialized = rmp_serde::to_vec(&CachedDiskValueRef::new(value))
            .map_err(|source| RedbCacheError::CacheSerialization { source });
        async move {
            let serialized = serialized?;
            blocking::unblock(move || disk_cache_set::<V>(&connection, &key, serialized, durable))
                .await
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::time::Duration;
    use googletest::{
        assert_that,
        matchers::{anything, eq, none, ok, some},
    };
    use std::thread::sleep;
    use tempfile::TempDir;

    use super::*;

    macro_rules! temp_dir {
        () => {
            TempDir::new().expect("Error creating temp dir")
        };
    }

    #[test]
    fn ttl_secs_and_ttl_millis_set_duration() {
        // No disk needed -- inspect the builder's ttl field without calling build().
        let b = RedbCache::<u32, u32>::builder()
            .name("ttl-secs-builder")
            .ttl_secs(7);
        assert_eq!(b.ttl, Some(Duration::from_secs(7)));

        let b = RedbCache::<u32, u32>::builder()
            .name("ttl-millis-builder")
            .ttl_millis(250);
        assert_eq!(b.ttl, Some(Duration::from_millis(250)));
    }

    #[test]
    fn ttl_setters_override_last_writer_wins() {
        // ttl(secs=10) then ttl_secs(5) -> 5s
        let b = RedbCache::<u32, u32>::builder()
            .name("ttl-override-a")
            .ttl(Duration::from_secs(10))
            .ttl_secs(5);
        assert_eq!(b.ttl, Some(Duration::from_secs(5)));

        // ttl_secs then ttl_millis -> the millis value
        let b = RedbCache::<u32, u32>::builder()
            .name("ttl-override-b")
            .ttl_secs(10)
            .ttl_millis(500);
        assert_eq!(b.ttl, Some(Duration::from_millis(500)));

        // ttl_millis then ttl -> the ttl value
        let b = RedbCache::<u32, u32>::builder()
            .name("ttl-override-c")
            .ttl_millis(500)
            .ttl(Duration::from_secs(3));
        assert_eq!(b.ttl, Some(Duration::from_secs(3)));
    }

    #[test]
    fn new_returns_ready_cache_via_builder_with_ttl_secs() {
        // RedbCache has no `new()` (builder-only); the ttl_secs convenience
        // setter produces a working disk cache that respects the TTL.
        let dir = temp_dir!();
        let cache: RedbCache<u32, u32> = RedbCache::builder()
            .name("ttl-secs-roundtrip")
            .disk_directory(dir.path())
            .ttl_secs(60)
            .build()
            .expect("build must succeed");
        assert_eq!(cache.cache_set(1, 100).unwrap(), None);
        assert_eq!(cache.cache_get(&1).unwrap(), Some(100));
    }

    #[test]
    fn set_ttl_zero_disables_expiry() {
        // `set_ttl(Duration::ZERO)` must disable expiry (== `unset_ttl`), not make
        // entries expire immediately: an entry written under a short ttl survives well
        // past it once expiry is disabled.
        let dir = temp_dir!();
        let cache: RedbCache<u32, u32> = RedbCache::builder()
            .name("set-ttl-zero-disables")
            .disk_directory(dir.path())
            .ttl_millis(20)
            .build()
            .expect("build must succeed");
        assert_eq!(cache.cache_set(1, 100).unwrap(), None);
        // Disabling returns the prior ttl, and `ttl()` then reports `None`.
        assert_eq!(
            cache.set_ttl(Duration::ZERO),
            Some(Duration::from_millis(20))
        );
        assert_eq!(cache.ttl(), None);
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(cache.cache_get(&1).unwrap(), Some(100));
    }

    // ── Test helpers for poking raw bytes into / out of the redb table ──────
    //
    // Used to plant corrupt/fixture bytes directly. They operate on the same
    // `TABLE` the cache uses.
    fn raw_insert(
        cache: &RedbCache<u32, impl Serialize + DeserializeOwned>,
        key: &str,
        value: Vec<u8>,
    ) {
        let wtxn = cache
            .connection
            .begin_write()
            .expect("error beginning write txn");
        {
            let mut table = wtxn.open_table(TABLE).expect("error opening table");
            table
                .insert(key, value.as_slice())
                .expect("error inserting fixture");
        }
        wtxn.commit().expect("error committing fixture");
    }

    fn raw_get(
        cache: &RedbCache<u32, impl Serialize + DeserializeOwned>,
        key: &str,
    ) -> Option<Vec<u8>> {
        let rtxn = cache
            .connection
            .begin_read()
            .expect("error beginning read txn");
        let table = rtxn.open_table(TABLE).expect("error opening table");
        table
            .get(key)
            .expect("error reading fixture")
            .map(|guard| guard.value().to_vec())
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
        let cache: RedbCache<u32, SerializeFailsAfterDeserialize> = RedbCache::builder()
            .name("serialize_error_on_refresh")
            .disk_directory(tmp_dir.path())
            .ttl(Duration::from_secs(10))
            .refresh_on_hit(true)
            .build()
            .expect("error building disk cache");
        let cached = CachedDiskValue::new(SerializeFailsAfterDeserialize { fail: false });
        raw_insert(
            &cache,
            &TEST_KEY.to_string(),
            rmp_serde::to_vec(&cached).expect("error serializing fixture"),
        );

        assert!(matches!(
            cache.cache_get(&TEST_KEY),
            Err(RedbCacheError::CacheSerialization { .. })
        ));
    }

    #[test]
    fn cache_get_returns_decode_error_for_corrupted_value() {
        let tmp_dir = temp_dir!();
        let cache: RedbCache<u32, u32> = RedbCache::builder()
            .name("corrupted-cache-get")
            .disk_directory(tmp_dir.path())
            .build()
            .expect("error building disk cache");
        raw_insert(&cache, &TEST_KEY.to_string(), vec![0xc1, 0xc1, 0xc1]);

        assert!(matches!(
            cache.cache_get(&TEST_KEY),
            Err(RedbCacheError::CacheDeserialization { .. })
        ));
        assert!(raw_get(&cache, &TEST_KEY.to_string()).is_some());
    }

    #[test]
    fn cache_delete_removes_corrupted_value_without_decoding() {
        let tmp_dir = temp_dir!();
        let cache: RedbCache<u32, u32> = RedbCache::builder()
            .name("corrupted-cache-delete")
            .disk_directory(tmp_dir.path())
            .build()
            .expect("error building disk cache");
        raw_insert(&cache, &TEST_KEY.to_string(), vec![0xc1, 0xc1, 0xc1]);

        assert!(cache.cache_delete(&TEST_KEY).unwrap());
        assert!(!cache.cache_delete(&TEST_KEY).unwrap());
        assert_that!(cache.cache_get(&TEST_KEY), ok(none()));
    }

    #[test]
    fn cache_set_overwrites_corrupted_value() {
        let tmp_dir = temp_dir!();
        let cache: RedbCache<u32, u32> = RedbCache::builder()
            .name("corrupted-cache-set")
            .disk_directory(tmp_dir.path())
            .build()
            .expect("error building disk cache");
        raw_insert(&cache, &TEST_KEY.to_string(), vec![0xc1, 0xc1, 0xc1]);

        // Setting over a corrupt previous value succeeds: the new value is written
        // and the undecodable previous value is reported as `None` (not an error).
        assert_that!(cache.cache_set(TEST_KEY, TEST_VAL), ok(none()));
        assert_that!(cache.cache_get(&TEST_KEY), ok(some(eq(&TEST_VAL))));
    }

    #[test]
    fn cache_remove_removes_corrupted_value() {
        let tmp_dir = temp_dir!();
        let cache: RedbCache<u32, u32> = RedbCache::builder()
            .name("corrupted-cache-remove")
            .disk_directory(tmp_dir.path())
            .build()
            .expect("error building disk cache");
        raw_insert(&cache, &TEST_KEY.to_string(), vec![0xc1, 0xc1, 0xc1]);

        // Removing a corrupt value succeeds: the entry is physically removed and
        // the undecodable value is reported as `None` (not an error).
        assert_that!(cache.cache_remove(&TEST_KEY), ok(none()));
        assert!(raw_get(&cache, &TEST_KEY.to_string()).is_none());
    }

    #[test]
    fn cache_remove_entry_round_trips_and_removes_corrupted_value() {
        let tmp_dir = temp_dir!();
        let cache: RedbCache<u32, u32> = RedbCache::builder()
            .name("remove-entry-roundtrip")
            .disk_directory(tmp_dir.path())
            .build()
            .expect("error building disk cache");

        // A decodable entry comes back as the `(key, value)` pair and is removed.
        cache.cache_set(TEST_KEY, TEST_VAL).unwrap();
        assert_eq!(
            cache.cache_remove_entry(&TEST_KEY).unwrap(),
            Some((TEST_KEY, TEST_VAL))
        );
        assert!(raw_get(&cache, &TEST_KEY.to_string()).is_none());
        // Removing a now-missing key reports `None`.
        assert_eq!(cache.cache_remove_entry(&TEST_KEY).unwrap(), None);

        // A corrupt stored value is removed without error and its undecodable
        // value reported as `None` (the documented `cache_remove_entry` behavior).
        raw_insert(&cache, &TEST_KEY.to_string(), vec![0xc1, 0xc1, 0xc1]);
        assert_that!(cache.cache_remove_entry(&TEST_KEY), ok(none()));
        assert!(raw_get(&cache, &TEST_KEY.to_string()).is_none());
    }

    #[test]
    fn cache_remove_entry_returns_expired_but_present_entry() {
        let tmp_dir = temp_dir!();
        let cache: RedbCache<u32, u32> = RedbCache::builder()
            .name("remove-entry-expired")
            .disk_directory(tmp_dir.path())
            .ttl(LIFE_SPAN_1_SEC)
            .build()
            .expect("error building disk cache");

        cache.cache_set(TEST_KEY, TEST_VAL).unwrap();
        cache.cache_set(2, TEST_VAL_1).unwrap();
        sleep(LIFE_SPAN_1_SEC + Duration::from_millis(50));

        // `cache_remove` honors the TTL: an expired entry reads back as `None`.
        assert_eq!(cache.cache_remove(&TEST_KEY).unwrap(), None);
        // `cache_remove_entry` does not filter by TTL: it returns the stored
        // `(key, value)` of an expired-but-present entry — the distinguishing
        // contract documented on `ConcurrentCached::cache_remove_entry`.
        assert_eq!(cache.cache_remove_entry(&2).unwrap(), Some((2, TEST_VAL_1)));
    }

    #[test]
    fn flush_forces_durable_commit_and_preserves_data() {
        let tmp_dir = temp_dir!();
        // Opt into Durability::None writes so flush() has buffered writes to persist.
        let cache: RedbCache<u32, u32> = RedbCache::builder()
            .name("flush-test")
            .disk_directory(tmp_dir.path())
            .durable(false)
            .build()
            .expect("error building disk cache");

        cache.cache_set(TEST_KEY, TEST_VAL).unwrap();
        cache.cache_set(2, TEST_VAL_1).unwrap();

        // flush forces a durable commit; safe to call repeatedly / with no new writes.
        cache.flush().expect("flush should succeed");
        cache.flush().expect("flush is idempotent");

        // entries remain readable after flushing
        assert_that!(cache.cache_get(&TEST_KEY), ok(some(eq(&TEST_VAL))));
        assert_that!(cache.cache_get(&2), ok(some(eq(&TEST_VAL_1))));

        // drop (releasing redb's file lock) and reopen the same file: the flushed
        // writes are present. (The fsync itself is not observable from a graceful
        // in-process reopen, so this checks the round-trip, not crash durability.)
        drop(cache);
        let reopened: RedbCache<u32, u32> = RedbCache::builder()
            .name("flush-test")
            .disk_directory(tmp_dir.path())
            .build()
            .expect("error re-opening cache");
        assert_that!(reopened.cache_get(&TEST_KEY), ok(some(eq(&TEST_VAL))));

        // flush is a safe no-op on an already-durable cache, and on an empty cache.
        let durable: RedbCache<u32, u32> = RedbCache::builder()
            .name("flush-test-durable")
            .disk_directory(tmp_dir.path())
            .durable(true)
            .build()
            .unwrap();
        durable
            .flush()
            .expect("flush on a durable/empty cache should succeed");
    }

    #[test]
    fn flush_makes_durability_none_writes_visible_to_a_fresh_instance() {
        // redb takes an exclusive lock on its file, so two instances cannot open the
        // same path at once. Instead we copy the `.redb` file (a crash-consistent
        // snapshot of the durable state) and open a fresh instance on the copy, which
        // only sees writes that have been made durable. A `Durability::None` write
        // (durable = false) must not appear in the snapshot until
        // `flush()` makes it durable.
        const NAME: &str = "flush-visibility";
        let file_name = format!("{NAME}_v{DISK_FILE_VERSION}.redb");

        let dir_a = temp_dir!();
        let src = dir_a.path().join(&file_name);
        let a: RedbCache<u32, u32> = RedbCache::builder()
            .name(NAME)
            .disk_directory(dir_a.path())
            .durable(false) // opt into Durability::None writes
            .build()
            .unwrap();
        a.cache_set(TEST_KEY, TEST_VAL).unwrap(); // Durability::None (not yet durable)

        // Snapshot before flush: a fresh instance on the copy must NOT see the entry.
        let dir_before = temp_dir!();
        std::fs::copy(&src, dir_before.path().join(&file_name)).unwrap();
        let before: RedbCache<u32, u32> = RedbCache::builder()
            .name(NAME)
            .disk_directory(dir_before.path())
            .build()
            .unwrap();
        assert_that!(
            before.cache_get(&TEST_KEY),
            ok(none()),
            "an un-flushed Durability::None write must not be durable"
        );

        // Flush, then snapshot again: a fresh instance now sees the entry.
        a.flush().unwrap();
        let dir_after = temp_dir!();
        std::fs::copy(&src, dir_after.path().join(&file_name)).unwrap();
        let after: RedbCache<u32, u32> = RedbCache::builder()
            .name(NAME)
            .disk_directory(dir_after.path())
            .build()
            .unwrap();
        assert_that!(
            after.cache_get(&TEST_KEY),
            ok(some(eq(&TEST_VAL))),
            "after flush the write is durable and visible to a fresh instance"
        );
    }

    #[test]
    fn remove_expired_entries_returns_decode_error_for_corrupted_value() {
        let tmp_dir = temp_dir!();
        let cache: RedbCache<u32, u32> = RedbCache::builder()
            .name("corrupted-sweep")
            .disk_directory(tmp_dir.path())
            .ttl(Duration::from_secs(1))
            .build()
            .expect("error building disk cache");
        raw_insert(&cache, &TEST_KEY.to_string(), vec![0xc1, 0xc1, 0xc1]);

        assert!(matches!(
            cache.remove_expired_entries(),
            Err(RedbCacheError::CacheDeserialization { .. })
        ));
    }

    #[test]
    fn remove_expired_entries_returns_count_of_removed_entries() {
        let tmp_dir = temp_dir!();
        let cache: RedbCache<u32, u32> = RedbCache::builder()
            .name("sweep-count")
            .disk_directory(tmp_dir.path())
            .ttl(LIFE_SPAN_1_SEC)
            .build()
            .expect("error building disk cache");

        // Two entries created now will expire after the ttl.
        cache.cache_set(1, 10).unwrap();
        cache.cache_set(2, 20).unwrap();

        // Wait past the ttl, then add a fresh (still-live) entry.
        sleep(LIFE_SPAN_1_SEC + Duration::from_millis(50));
        cache.cache_set(3, 30).unwrap();

        // The sweep removes exactly the two expired entries and reports the count.
        assert_eq!(cache.remove_expired_entries().unwrap(), 2);
        // The live entry survives; the expired ones are physically gone.
        assert!(raw_get(&cache, &3u32.to_string()).is_some());
        assert!(raw_get(&cache, &1u32.to_string()).is_none());
        assert!(raw_get(&cache, &2u32.to_string()).is_none());
    }

    const LIFE_SPAN_2_SECS: Duration = Duration::from_secs(2);
    const LIFE_SPAN_1_SEC: Duration = Duration::from_secs(1);
    #[googletest::test]
    fn cache_get_after_cache_remove_returns_none() {
        let tmp_dir = temp_dir!();
        let cache: RedbCache<u32, u32> = RedbCache::builder()
            .name("test-cache")
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
    fn cache_clear_empties_the_table() {
        let tmp_dir = temp_dir!();
        let cache: RedbCache<u32, u32> = RedbCache::builder()
            .name("test-cache-clear")
            .disk_directory(tmp_dir.path())
            .build()
            .unwrap();

        cache.cache_set(TEST_KEY, TEST_VAL).unwrap();
        cache.cache_set(TEST_KEY + 1, TEST_VAL_1).unwrap();

        cache.cache_clear().expect("error clearing cache");

        assert_that!(
            cache.cache_get(&TEST_KEY),
            ok(none()),
            "Getting a key after cache_clear should return None"
        );
        assert_that!(
            cache.cache_get(&(TEST_KEY + 1)),
            ok(none()),
            "Getting a second key after cache_clear should return None"
        );
    }

    #[googletest::test]
    fn values_expire_when_lifespan_elapses_returning_none() {
        let tmp_dir = temp_dir!();
        let cache: RedbCache<u32, u32> = RedbCache::builder()
            .name("test-cache")
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
        let cache: RedbCache<u32, u32> = RedbCache::builder()
            .name("test-cache")
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
            ConcurrentCacheTtl::set_ttl(&cache, LIFE_SPAN_1_SEC).expect("error setting new ttl");
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
            ok(some(eq(&TEST_VAL))),
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

        ConcurrentCacheTtl::set_ttl(&cache, Duration::from_secs(10)).expect("error setting ttl");
        assert_that!(
            cache.cache_set(TEST_KEY, TEST_VAL),
            ok(none()),
            "Setting a previously expired key-value should return None"
        );

        assert_that!(
            cache.cache_get(&TEST_KEY),
            ok(some(eq(&TEST_VAL))),
            "Getting a newly set (previously expired) key-value should return the value"
        );
        assert_that!(
            cache.cache_get(&TEST_KEY),
            ok(some(eq(&TEST_VAL))),
            "Getting the same value again should return the value"
        );
    }

    #[googletest::test]
    fn refreshing_on_cache_get_delays_cache_expiry() {
        // NOTE: Here we're relying on the fact that setting then sleeping for 2 secs and getting takes longer than 2 secs.
        const LIFE_SPAN: Duration = LIFE_SPAN_2_SECS;
        const HALF_LIFE_SPAN: Duration = LIFE_SPAN_1_SEC;
        let tmp_dir = temp_dir!();
        let cache: RedbCache<u32, u32> = RedbCache::builder()
            .name("test-cache")
            .disk_directory(tmp_dir.path())
            .ttl(LIFE_SPAN)
            .refresh_on_hit(true) // ENABLE REFRESH - this is what we're testing
            .build()
            .unwrap();

        assert_that!(cache.cache_set(TEST_KEY, TEST_VAL), ok(none()));

        // retrieve before expiry, this should refresh the created_at so we don't expire just yet
        sleep(HALF_LIFE_SPAN);
        assert_that!(
            cache.cache_get(&TEST_KEY),
            ok(some(eq(&TEST_VAL))),
            "Getting a value before expiry should return the value"
        );

        // This is after the initial expiry, but since we refreshed the created_at, we should still get the value
        sleep(HALF_LIFE_SPAN);
        assert_that!(
            cache.cache_get(&TEST_KEY),
            ok(some(eq(&TEST_VAL))),
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
        let cache: RedbCache<u32, u32> = RedbCache::builder()
            .name(format!("{}:disk-cache-test-default-dir", now_millis()))
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

        // remove the cache file to clean up the test as we're not using a temp dir
        std::fs::remove_file(cache.disk_path()).expect("error in clean up removing the cache file")
    }

    mod set_durable {

        mod persistence_across_reopen {
            use super::super::*;

            /// Build a cache, run `run_on_original_cache`, then re-open the SAME
            /// on-disk redb file in a fresh `RedbCache` and run
            /// `run_on_recovered_cache` against it. This verifies what is
            /// readable from the persisted file.
            ///
            /// With redb there is no separate flush step: a committed write txn
            /// is written into the file (durability only governs whether the
            /// write is fsync'd). Re-opening the same file in-process therefore
            /// observes all committed writes regardless of the durability
            /// setting. `Durability::None` vs `Durability::Immediate` differ
            /// only in whether an fsync is issued, which is not observable from a
            /// graceful in-process reopen. We therefore assert persistence for
            /// both `durable = true` and `durable = false`; the fsync difference is
            /// not deterministically testable without a real crash/power-loss harness.
            fn check_on_recovered_cache(
                set_durable: bool,
                run_on_original_cache: fn(&RedbCache<u32, u32>) -> (),
                run_on_recovered_cache: fn(&RedbCache<u32, u32>) -> (),
            ) {
                let cache_tmp_dir = temp_dir!();
                const CACHE_NAME: &str = "test-cache";

                {
                    let cache: RedbCache<u32, u32> = RedbCache::builder()
                        .name(CACHE_NAME)
                        .disk_directory(cache_tmp_dir.path())
                        .durable(set_durable) // WHAT'S BEING TESTED
                        .build()
                        .unwrap();

                    run_on_original_cache(&cache);
                    // Drop the original cache so its exclusive lock on the redb
                    // file is released before we re-open it below.
                }

                let recovered_cache: RedbCache<u32, u32> = RedbCache::builder()
                    .name(CACHE_NAME)
                    .disk_directory(cache_tmp_dir.path())
                    .durable(set_durable)
                    .build()
                    .expect("error re-opening cache from persisted file");

                run_on_recovered_cache(&recovered_cache);
            }

            mod changes_persist_after_recovery {
                use super::*;

                #[googletest::test]
                fn for_cache_set() {
                    check_on_recovered_cache(
                        true,
                        |cache| {
                            cache
                                .cache_set(TEST_KEY, TEST_VAL)
                                .expect("error setting cache in assemble stage");
                        },
                        |recovered_cache| {
                            assert_that!(
                                recovered_cache.cache_get(&TEST_KEY),
                                ok(some(eq(&TEST_VAL))),
                                "Getting a set key should return the value after re-opening the file"
                            );
                        },
                    )
                }

                #[googletest::test]
                fn for_cache_remove() {
                    check_on_recovered_cache(
                        true,
                        |cache| {
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
                                "Getting a removed key should return None after re-opening the file"
                            );
                        },
                    )
                }
            }

            mod changes_persist_after_recovery_non_durable {
                use super::*;

                #[googletest::test]
                fn for_cache_set() {
                    check_on_recovered_cache(
                        false,
                        |cache| {
                            cache
                                .cache_set(TEST_KEY, TEST_VAL)
                                .expect("error setting cache in assemble stage");
                        },
                        |recovered_cache| {
                            assert_that!(
                                recovered_cache.cache_get(&TEST_KEY),
                                ok(some(eq(&TEST_VAL))),
                                "Getting a set key should return the value after re-opening the file"
                            );
                        },
                    )
                }

                #[googletest::test]
                fn for_cache_remove() {
                    check_on_recovered_cache(
                        false,
                        |cache| {
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
                                "Getting a removed key should return None after re-opening the file"
                            );
                        },
                    )
                }
            }
        }
    }

    /// Exercises the `ConcurrentCachedAsync` (`async_*`) path for `RedbCache`,
    /// which the synchronous tests above do not cover: TTL expiry via
    /// `async_cache_get`, and set/remove/delete round-trips through the async API.
    #[cfg(feature = "async")]
    #[tokio::test]
    async fn async_path_respects_ttl_and_round_trips() {
        use crate::ConcurrentCachedAsync;

        let tmp_dir = temp_dir!();
        let cache: RedbCache<u32, u32> = RedbCache::builder()
            .name("test-cache-async")
            .disk_directory(tmp_dir.path())
            .ttl(LIFE_SPAN_1_SEC)
            .build()
            .unwrap();

        // set returns the previous value (None for a new key)
        assert_eq!(
            cache.async_cache_set(TEST_KEY, TEST_VAL).await.unwrap(),
            None
        );
        // live read through the async path
        assert_eq!(
            cache.async_cache_get(&TEST_KEY).await.unwrap(),
            Some(TEST_VAL)
        );

        // once the TTL elapses, the async read evicts the entry and returns None.
        // Use tokio's timer (not std::thread::sleep) so we don't block the executor.
        tokio::time::sleep(LIFE_SPAN_1_SEC + Duration::from_millis(50)).await;
        assert_eq!(cache.async_cache_get(&TEST_KEY).await.unwrap(), None);

        // remove / delete round-trips via the async path
        assert_eq!(
            cache.async_cache_set(TEST_KEY, TEST_VAL).await.unwrap(),
            None
        );
        assert_eq!(
            cache.async_cache_remove(&TEST_KEY).await.unwrap(),
            Some(TEST_VAL)
        );
        assert!(!cache.async_cache_delete(&TEST_KEY).await.unwrap());

        // async_cache_clear empties the table (and leaves it readable afterward)
        cache.async_cache_set(TEST_KEY, TEST_VAL).await.unwrap();
        cache.async_cache_set(2, TEST_VAL_1).await.unwrap();
        cache.async_cache_clear().await.unwrap();
        assert_eq!(cache.async_cache_get(&TEST_KEY).await.unwrap(), None);
        assert_eq!(cache.async_cache_get(&2).await.unwrap(), None);
    }

    #[cfg(feature = "async")]
    #[tokio::test]
    async fn async_cache_remove_entry_round_trips_and_removes_corrupted_value() {
        use crate::ConcurrentCachedAsync;

        let tmp_dir = temp_dir!();
        let cache: RedbCache<u32, u32> = RedbCache::builder()
            .name("remove-entry-async")
            .disk_directory(tmp_dir.path())
            .build()
            .unwrap();

        // A decodable entry comes back as the `(key, value)` pair and is removed.
        cache.async_cache_set(TEST_KEY, TEST_VAL).await.unwrap();
        assert_eq!(
            cache.async_cache_remove_entry(&TEST_KEY).await.unwrap(),
            Some((TEST_KEY, TEST_VAL))
        );
        assert!(raw_get(&cache, &TEST_KEY.to_string()).is_none());
        // Removing a now-missing key reports `None`.
        assert_eq!(
            cache.async_cache_remove_entry(&TEST_KEY).await.unwrap(),
            None
        );

        // A corrupt stored value is removed without error and its undecodable
        // value reported as `None`, matching the sync `cache_remove_entry`.
        raw_insert(&cache, &TEST_KEY.to_string(), vec![0xc1, 0xc1, 0xc1]);
        assert_eq!(
            cache.async_cache_remove_entry(&TEST_KEY).await.unwrap(),
            None
        );
        assert!(raw_get(&cache, &TEST_KEY.to_string()).is_none());
    }

    #[test]
    fn cache_set_ref_round_trips() {
        let tmp_dir = temp_dir!();
        let cache: RedbCache<u32, u32> = RedbCache::builder()
            .name("set-ref-roundtrip")
            .disk_directory(tmp_dir.path())
            .build()
            .expect("error building disk cache");

        let key = TEST_KEY;
        let value = TEST_VAL;
        // cache_set_ref writes from a borrow; the previous value is None.
        assert_that!(
            crate::SerializeCached::cache_set_ref(&cache, &key, &value),
            ok(none()),
            "cache_set_ref on a new key should return None"
        );
        // cache_get must return the value that was written via cache_set_ref.
        assert_that!(
            cache.cache_get(&key),
            ok(some(eq(&value))),
            "cache_get after cache_set_ref should return the written value"
        );
        // A second cache_set_ref displaces the first and returns it.
        let value2 = TEST_VAL_1;
        assert_that!(
            crate::SerializeCached::cache_set_ref(&cache, &key, &value2),
            ok(some(eq(&value))),
            "cache_set_ref over an existing entry should return the old value"
        );
        assert_that!(
            cache.cache_get(&key),
            ok(some(eq(&value2))),
            "cache_get should return the most recently set value"
        );
    }

    #[test]
    fn debug_smoke_exposes_non_secret_fields_only() {
        let tmp_dir = temp_dir!();
        let cache: RedbCache<u32, u32> = RedbCache::builder()
            .name("debug-smoke")
            .disk_directory(tmp_dir.path())
            .ttl_secs(60)
            .refresh_on_hit(true)
            .build()
            .expect("error building disk cache");

        let s = format!("{:?}", cache);
        assert!(!s.is_empty(), "Debug output must be non-empty");
        // Type name and the non-secret config fields must be present.
        assert!(s.contains("RedbCache"), "Debug must name the type: {s}");
        assert!(s.contains("ttl"), "Debug must show ttl: {s}");
        assert!(s.contains("refresh"), "Debug must show refresh: {s}");
        assert!(s.contains("durable"), "Debug must show durable: {s}");
        // finish_non_exhaustive renders a trailing `..`.
        assert!(
            s.contains(".."),
            "Debug must be non-exhaustive (trailing ..): {s}"
        );
        // The private `connection` (live `Database` handle) must not be named.
        assert!(
            !s.contains("connection"),
            "Debug must not expose the connection handle: {s}"
        );
        // Guard against a future regression that leaks a redis-style
        // connection string from a disk cache that has none.
        assert!(
            !s.contains("redis://") && !s.contains("rediss://"),
            "Debug must not contain a connection scheme: {s}"
        );
    }

    #[test]
    fn build_rejects_cache_name_with_path_separator_or_dot_components() {
        let tmp_dir = temp_dir!();

        assert!(
            matches!(
                RedbCache::<u32, u32>::builder()
                    .name("")
                    .disk_directory(tmp_dir.path())
                    .build(),
                Err(RedbCacheBuildError::InvalidCacheName)
            ),
            "empty cache_name must return InvalidCacheName"
        );

        assert!(
            matches!(
                RedbCache::<u32, u32>::builder()
                    .name("bad/name")
                    .disk_directory(tmp_dir.path())
                    .build(),
                Err(RedbCacheBuildError::InvalidCacheName)
            ),
            "cache_name containing '/' must return InvalidCacheName"
        );

        // ':' is allowed (established usage in module-path / timestamp-derived names).
        assert!(
            RedbCache::<u32, u32>::builder()
                .name("ok:name")
                .disk_directory(tmp_dir.path())
                .build()
                .is_ok(),
            "cache_name containing ':' must be accepted"
        );

        assert!(
            matches!(
                RedbCache::<u32, u32>::builder()
                    .name("bad\\name")
                    .disk_directory(tmp_dir.path())
                    .build(),
                Err(RedbCacheBuildError::InvalidCacheName)
            ),
            "cache_name containing '\\\\' must return InvalidCacheName"
        );

        assert!(
            matches!(
                RedbCache::<u32, u32>::builder()
                    .name("..")
                    .disk_directory(tmp_dir.path())
                    .build(),
                Err(RedbCacheBuildError::InvalidCacheName)
            ),
            "cache_name '..' must return InvalidCacheName"
        );

        assert!(
            matches!(
                RedbCache::<u32, u32>::builder()
                    .name(".")
                    .disk_directory(tmp_dir.path())
                    .build(),
                Err(RedbCacheBuildError::InvalidCacheName)
            ),
            "cache_name '.' must return InvalidCacheName"
        );

        // A valid name must still build successfully.
        assert!(
            RedbCache::<u32, u32>::builder()
                .name("valid-cache-name")
                .disk_directory(tmp_dir.path())
                .build()
                .is_ok(),
            "a valid cache_name must build successfully"
        );
    }

    #[test]
    fn build_rejects_cache_name_with_nul_byte() {
        let tmp_dir = temp_dir!();

        assert!(
            matches!(
                RedbCache::<u32, u32>::builder()
                    .name("bad\0name")
                    .disk_directory(tmp_dir.path())
                    .build(),
                Err(RedbCacheBuildError::InvalidCacheName)
            ),
            "cache_name containing a NUL byte must return InvalidCacheName"
        );
    }

    #[cfg(feature = "async")]
    #[tokio::test]
    async fn async_cache_set_ref_round_trips() {
        use crate::SerializeCachedAsync;

        let tmp_dir = temp_dir!();
        let cache: RedbCache<u32, u32> = RedbCache::builder()
            .name("set-ref-roundtrip-async")
            .disk_directory(tmp_dir.path())
            .build()
            .expect("error building disk cache");

        let key = TEST_KEY;
        let value = TEST_VAL;
        // async_cache_set_ref writes from a borrow; the previous value is None.
        assert_eq!(
            cache.async_cache_set_ref(&key, &value).await.unwrap(),
            None,
            "async_cache_set_ref on a new key should return None"
        );
        // async_cache_get must return the value that was written via async_cache_set_ref.
        use crate::ConcurrentCachedAsync;
        assert_eq!(
            cache.async_cache_get(&key).await.unwrap(),
            Some(value),
            "async_cache_get after async_cache_set_ref should return the written value"
        );
        // A second async_cache_set_ref displaces the first.
        let value2 = TEST_VAL_1;
        assert_eq!(
            cache.async_cache_set_ref(&key, &value2).await.unwrap(),
            Some(value),
            "async_cache_set_ref over an existing entry should return the old value"
        );
        assert_eq!(
            cache.async_cache_get(&key).await.unwrap(),
            Some(value2),
            "async_cache_get should return the most recently set value"
        );
    }

    #[cfg(feature = "async")]
    #[tokio::test]
    async fn async_flush_succeeds_and_preserves_data() {
        use crate::ConcurrentCachedAsync;

        let tmp_dir = temp_dir!();
        let cache: RedbCache<u32, u32> = RedbCache::builder()
            .name("flush-test-async")
            .disk_directory(tmp_dir.path())
            .build()
            .unwrap();

        cache.async_cache_set(TEST_KEY, TEST_VAL).await.unwrap();
        cache
            .async_flush()
            .await
            .expect("async_flush should succeed");
        assert_eq!(
            cache.async_cache_get(&TEST_KEY).await.unwrap(),
            Some(TEST_VAL)
        );
    }

    /// Prove runtime-agnosticism: run async RedbCache operations under
    /// `futures::executor::block_on` (a minimal single-threaded executor, no
    /// tokio). The `blocking` crate uses its own thread pool, so the blocking
    /// redb I/O executes correctly regardless of which async executor drives the
    /// future.
    #[cfg(feature = "async")]
    #[test]
    fn async_redb_cache_works_under_futures_block_on() {
        use crate::ConcurrentCachedAsync;
        use futures::executor::block_on;

        let tmp_dir = temp_dir!();
        let cache: RedbCache<u32, u32> = RedbCache::builder()
            .name("futures-block-on-test")
            .disk_directory(tmp_dir.path())
            .build()
            .unwrap();

        // set then get via a non-tokio executor
        let prev = block_on(cache.async_cache_set(TEST_KEY, TEST_VAL)).unwrap();
        assert_eq!(prev, None, "first set returns no prior value");
        let got = block_on(cache.async_cache_get(&TEST_KEY)).unwrap();
        assert_eq!(got, Some(TEST_VAL), "get returns the value that was set");

        // remove via the non-tokio executor
        let removed = block_on(cache.async_cache_remove(&TEST_KEY)).unwrap();
        assert_eq!(
            removed,
            Some(TEST_VAL),
            "remove returns the previously set value"
        );
        let after = block_on(cache.async_cache_get(&TEST_KEY)).unwrap();
        assert_eq!(after, None, "get after remove returns None");

        // async_flush also works
        block_on(cache.async_flush()).expect("async_flush under futures::block_on should succeed");
    }

    // ── Error variant shape and naming tests ─────────────────────────────────
    //
    // These tests assert the renamed/reshaped variants introduced in item 0005:
    // - `RedbCacheBuildError::Storage` (renamed from `Connection`)
    // - struct variants with named fields on both error enums
    // - `CacheDeserialization::cached_value` carries the raw bytes that failed
    //   to decode

    /// `RedbCacheBuildError::Storage` (renamed from `Connection`) is produced
    /// by build-time redb failures. Its Display no longer says "connection".
    #[test]
    fn build_error_storage_variant_name_and_display() {
        // Construct the variant directly to verify the field name compiles.
        let err = RedbCacheBuildError::Storage {
            source: redb::Error::Io(std::io::Error::other("synthetic redb io error")),
        };
        let display = err.to_string();
        // Must say "storage" (case-insensitive).
        assert!(
            display.to_lowercase().contains("storage"),
            "Storage variant display must mention storage: {display}"
        );
        // Must NOT say "connection" (the old, misleading word).
        assert!(
            !display.to_lowercase().contains("connection"),
            "Storage variant display must not mention connection: {display}"
        );
    }

    /// `RedbCacheError::CacheDeserialization` is a struct variant. The
    /// `cached_value` field carries the exact bytes that failed to decode,
    /// and `source` holds the underlying decode error.
    #[test]
    fn cache_get_decode_error_carries_raw_bytes() {
        let tmp_dir = temp_dir!();
        let cache: RedbCache<u32, u32> = RedbCache::builder()
            .name("decode-error-carries-bytes")
            .disk_directory(tmp_dir.path())
            .build()
            .expect("error building disk cache");
        let corrupt: Vec<u8> = vec![0xc1, 0xc1, 0xc1];
        raw_insert(&cache, &TEST_KEY.to_string(), corrupt.clone());

        match cache.cache_get(&TEST_KEY) {
            Err(RedbCacheError::CacheDeserialization {
                cached_value,
                source: _,
            }) => {
                assert_eq!(
                    cached_value, corrupt,
                    "cached_value must carry the exact bytes that failed to decode"
                );
            }
            other => panic!("expected CacheDeserialization, got {other:?}"),
        }
        // Entry must still be present (cache_get does not remove on decode error).
        assert!(raw_get(&cache, &TEST_KEY.to_string()).is_some());
    }

    /// `RedbCacheError::CacheDeserialization` from `remove_expired_entries`
    /// also carries the raw bytes via the `cached_value` field.
    #[test]
    fn remove_expired_entries_decode_error_carries_raw_bytes() {
        let tmp_dir = temp_dir!();
        let cache: RedbCache<u32, u32> = RedbCache::builder()
            .name("sweep-decode-error-bytes")
            .disk_directory(tmp_dir.path())
            .ttl(Duration::from_secs(1))
            .build()
            .expect("error building disk cache");
        let corrupt: Vec<u8> = vec![0xc1, 0xc1, 0xc1];
        raw_insert(&cache, &TEST_KEY.to_string(), corrupt.clone());

        match cache.remove_expired_entries() {
            Err(RedbCacheError::CacheDeserialization {
                cached_value,
                source: _,
            }) => {
                assert_eq!(
                    cached_value, corrupt,
                    "cached_value must carry the exact bytes that failed to decode"
                );
            }
            other => panic!("expected CacheDeserialization, got {other:?}"),
        }
    }

    /// `RedbCacheError::CacheSerialization` is a struct variant with a `source`
    /// field. Verify that the variant can be constructed and matched with named
    /// fields (not a bare tuple wildcard).
    #[test]
    fn cache_serialization_error_is_struct_variant() {
        let tmp_dir = temp_dir!();
        let cache: RedbCache<u32, SerializeFailsAfterDeserialize> = RedbCache::builder()
            .name("ser-error-struct-variant")
            .disk_directory(tmp_dir.path())
            .ttl(Duration::from_secs(10))
            .refresh_on_hit(true)
            .build()
            .expect("error building disk cache");
        let fixture = CachedDiskValue::new(SerializeFailsAfterDeserialize { fail: false });
        raw_insert(
            &cache,
            &TEST_KEY.to_string(),
            rmp_serde::to_vec(&fixture).expect("error serializing fixture"),
        );

        match cache.cache_get(&TEST_KEY) {
            Err(RedbCacheError::CacheSerialization { source: _ }) => {}
            other => panic!("expected CacheSerialization, got {other:?}"),
        }
    }

    /// `std::error::Error::source()` on `RedbCacheError::CacheDeserialization`
    /// must return the underlying decode error.
    #[test]
    fn cache_deserialization_error_source_is_wired() {
        use std::error::Error;
        let tmp_dir = temp_dir!();
        let cache: RedbCache<u32, u32> = RedbCache::builder()
            .name("deser-source-wired")
            .disk_directory(tmp_dir.path())
            .build()
            .expect("error building disk cache");
        raw_insert(&cache, &TEST_KEY.to_string(), vec![0xc1, 0xc1, 0xc1]);

        let err = cache
            .cache_get(&TEST_KEY)
            .expect_err("expected a decode error");
        assert!(
            err.source().is_some(),
            "CacheDeserialization must expose its inner error via source()"
        );
    }

    /// `RedbCacheBuildError::Storage`'s `source` field is wired as the
    /// `std::error::Error::source()` of the wrapper.
    #[test]
    fn build_error_storage_source_is_wired() {
        use std::error::Error;
        let inner = redb::Error::Io(std::io::Error::other("synthetic redb io error"));
        let err = RedbCacheBuildError::Storage { source: inner };
        assert!(
            err.source().is_some(),
            "RedbCacheBuildError::Storage must expose its inner error via source()"
        );
    }

    // ── Unix permission / security tests ─────────────────────────────────────
    //
    // Verifies that the cache directory is created with mode 0700, that the
    // redb database file is created with mode 0600, and that the temp_dir
    // fallback path is rejected if it resolves to a symlink.

    #[cfg(unix)]
    mod unix_permissions {
        use super::*;
        use std::os::unix::fs::MetadataExt;

        /// The cache directory created by `disk_directory(path)` must have
        /// mode 0700 (owner rwx only).
        #[test]
        fn explicit_disk_dir_is_created_with_mode_0700() {
            let parent = temp_dir!();
            // Point to a non-existent subdirectory so create_cache_dir must create it.
            let cache_dir = parent.path().join("sub").join("cache");
            let cache: RedbCache<u32, u32> = RedbCache::builder()
                .name("perm-dir-explicit")
                .disk_directory(&cache_dir)
                .build()
                .expect("build must succeed");
            let meta = std::fs::metadata(&cache_dir).expect("metadata");
            // Lower 9 bits: 0700 = 0o700
            assert_eq!(
                meta.mode() & 0o777,
                0o700,
                "cache directory must be created with mode 0700; got {:o}",
                meta.mode() & 0o777
            );
            drop(cache);
        }

        /// The redb database file must be created with mode 0600 (owner rw only).
        #[test]
        fn redb_file_is_created_with_mode_0600() {
            let dir = temp_dir!();
            let cache: RedbCache<u32, u32> = RedbCache::builder()
                .name("perm-file")
                .disk_directory(dir.path())
                .build()
                .expect("build must succeed");
            let file_path = cache.disk_path().to_owned();
            drop(cache); // release the redb exclusive lock before inspecting
            let meta = std::fs::metadata(&file_path).expect("metadata");
            assert_eq!(
                meta.mode() & 0o777,
                0o600,
                "redb file must be created with mode 0600; got {:o}",
                meta.mode() & 0o777
            );
        }

        /// A symlinked temp fallback directory must be rejected by
        /// `validate_temp_cache_dir` with a `PermissionDenied` error.
        #[test]
        fn symlinked_temp_fallback_dir_is_rejected() {
            let real_dir = temp_dir!();
            let link_dir = temp_dir!();
            let link_path = link_dir.path().join("symlink_cache");
            std::os::unix::fs::symlink(real_dir.path(), &link_path)
                .expect("failed to create symlink");
            // validate_temp_cache_dir must reject a symlink.
            let result = super::super::validate_temp_cache_dir(&link_path);
            assert!(
                result.is_err(),
                "validate_temp_cache_dir must reject a symlink"
            );
            let err = result.unwrap_err();
            assert_eq!(
                err.kind(),
                std::io::ErrorKind::PermissionDenied,
                "rejection must be PermissionDenied, got {err}"
            );
        }

        /// A real (non-symlink) temp fallback directory must be accepted by
        /// `validate_temp_cache_dir`.
        #[test]
        fn real_temp_fallback_dir_is_accepted() {
            let dir = temp_dir!();
            // Ensure the directory has mode 0700 (which create_cache_dir produces).
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
                .expect("set_permissions");
            let result = super::super::validate_temp_cache_dir(dir.path());
            assert!(
                result.is_ok(),
                "validate_temp_cache_dir must accept a real 0700 dir: {result:?}"
            );
        }

        /// A world-writable temp fallback directory must be rejected.
        #[test]
        fn world_writable_temp_fallback_dir_is_rejected() {
            let dir = temp_dir!();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o777))
                .expect("set_permissions");
            let result = super::super::validate_temp_cache_dir(dir.path());
            assert!(
                result.is_err(),
                "validate_temp_cache_dir must reject a world-writable dir"
            );
            assert_eq!(
                result.unwrap_err().kind(),
                std::io::ErrorKind::PermissionDenied
            );
        }
    }
}
