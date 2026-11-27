#[cfg(feature = "compression_zstd")]
use crate::stores::Compression;
use crate::stores::io_value_compression::{IoValueCompression, should_preserve_on_decode_error};
use crate::time::Duration;
use crate::time::SystemTime;
use crate::{
    ConcurrentCacheBase, ConcurrentCacheRefreshOnHit, ConcurrentCacheTtl, ConcurrentCached,
};
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
    strict_deserialization: bool,
    compression: IoValueCompression,
    // fn-pointer phantom — see the rationale on `RedbCache::_phantom`; keeps the
    // type unconditionally `Send + Sync` regardless of `K`/`V`.
    _phantom: PhantomData<fn() -> (K, V)>,
}

use thiserror::Error;

/// Error returned when building a [`RedbCache`].
///
/// Configuration problems (a missing `name`, or a zero `ttl`) surface as the transparent
/// [`Build`](Self::Build) variant wrapping a [`BuildError`](super::BuildError):
///
/// ```ignore
/// // `RedbCacheBuilder::new()` omits the required name, so `build` reports it.
/// match RedbCacheBuilder::<String, u32>::new().build() {
///     Err(RedbCacheBuildError::Build(BuildError::MissingRequired(field))) => { /* e.g. "name" */ }
///     Err(RedbCacheBuildError::Build(BuildError::InvalidValue { field, reason })) => { /* e.g. "ttl" */ }
///     _ => {}
/// }
/// ```
///
/// **Semver note:** the concrete source types inside `Storage` and `Io` are
/// NOT part of the public API and may change without a major version bump.
/// Inspect them via [`std::error::Error::source`] and downcast if needed.
#[non_exhaustive]
#[derive(Error, Debug)]
pub enum RedbCacheBuildError {
    #[error("storage error: {source}")]
    Storage {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
    #[error(transparent)]
    Build(#[from] super::BuildError),
    #[error("I/O error preparing the disk cache directory: {source}")]
    Io {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
    /// The `cache_name` passed to [`RedbCacheBuilder`] is invalid. It is used as a
    /// cross-platform filename component, so it must not be empty, must not be `.` or `..`,
    /// and must not contain any character that is invalid in a filename: a path separator
    /// (`/` or `\`), any of the NTFS/Windows-reserved characters `:` `<` `>` `"` `|` `?` `*`,
    /// or an ASCII control character (`0x00`-`0x1F` including NUL, and DEL `0x7F`).
    #[error(
        "invalid cache_name: must not be empty or '.'/'..', and must not contain characters \
        invalid in a filename (a path separator '/' or '\\\\', any of ':' '<' '>' '\"' '|' '?' '*', \
        or an ASCII control character); cache_name is used as a cross-platform filename component"
    )]
    InvalidCacheName,
}

impl RedbCacheBuildError {
    pub(crate) fn storage(e: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Storage {
            source: Box::new(e),
        }
    }
    pub(crate) fn io(e: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Io {
            source: Box::new(e),
        }
    }
}

static DISK_FILE_PREFIX: &str = "cached_disk_cache";
// Bumped whenever the on-disk format changes (the redb migration, then dropping the
// per-entry `version` field), so an incompatible older file is never read.
// Starts at 3 to distinguish from the sled-era DiskCache format (versions 1 and 2
// were the sled-backed DiskCache; those files are incompatible and are never read).
//
// Stability: the on-disk format (this version in the file name, the redb table
// name, and the MessagePack layout of `CachedDiskValue`) is stable for the 3.x
// series: files written by any 3.x release remain readable by every later 3.x
// release. Values are written with `rmp_serde::to_vec`, never `to_vec_named`, so
// the layout is a POSITIONAL array of `value` then `created_at`: the field names
// are not on the wire and the field ORDER is part of the frozen format.
// Reordering, inserting or removing a field of `CachedDiskValue` reinterprets
// every stored entry and is a format change like any other. A format change bumps
// this version (isolating old files rather than corrupting them) and is otherwise
// reserved for a major release.
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
            strict_deserialization: false,
            compression: IoValueCompression::default(),
            _phantom: Default::default(),
        }
    }

    /// Set the cache name (required). Used as a cross-platform filename component for the
    /// on-disk database file, so it must not be empty, must not be `.` or `..`, and must not
    /// contain a character invalid in a filename: a path separator (`/` or `\`), any of the
    /// NTFS/Windows-reserved characters `:` `<` `>` `"` `|` `?` `*`, or an ASCII control
    /// character. Note a module-path-style name such as `a::b` is rejected because of the `:`.
    #[must_use]
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.cache_name = Some(name.into());
        self
    }

    /// Specify the cache TTL as a `Duration`.
    ///
    /// **TTL is optional.** When no TTL is set (the default), entries never
    /// expire and are kept until explicitly removed or the cache is cleared.
    /// `RedbCache` tracks expiry client-side via a stored `created_at` timestamp,
    /// unlike [`RedisCache`](crate::stores::RedisCache), which relies on
    /// server-side TTL commands (`PSETEX`/`PEXPIRE`). Both stores make TTL
    /// optional in 3.0.
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

    /// Specify whether cache hits refresh the TTL.
    ///
    /// **Cost:** with `refresh_on_hit` enabled, every cache hit rewrites the entry's
    /// `created_at` in its own write transaction, so a read that would otherwise be a
    /// lock-free MVCC read becomes a serialized write (and, when `durable` is `true`, an
    /// fsync). On a read-heavy cache this per-hit write can dominate cost; leave it disabled
    /// unless you need sliding-expiry semantics.
    #[must_use]
    pub fn refresh_on_hit(mut self, refresh: bool) -> Self {
        self.refresh = refresh;
        self
    }

    /// Set the disk path for where the data will be stored.
    ///
    /// When not called, the builder falls back to a platform-appropriate default
    /// directory keyed by the executable name: the user cache dir (e.g.
    /// `$XDG_CACHE_HOME` / `~/.cache` on Linux) when writable, otherwise the
    /// system temp dir. Data in a temp dir may be cleaned up by the OS; set an
    /// explicit directory for anything that should persist. The resolved path is
    /// available via [`RedbCache::disk_path`] after building.
    #[must_use]
    pub fn disk_dir<P: AsRef<Path>>(mut self, dir: P) -> Self {
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

    /// Set whether deserialization failures are propagated as errors (`true`) or
    /// silently self-healed (`false`, the default).
    ///
    /// **Default (`false`, self-heal mode):** when a stored entry cannot be decoded
    /// (e.g. after a format migration or data corruption), `cache_get` deletes the
    /// offending entry and returns `Ok(None)` — a transparent cache miss that lets
    /// the caller repopulate the entry. `remove_expired_entries` treats undecodable
    /// entries as eviction candidates and removes them (counting each one).
    ///
    /// **Strict mode (`true`):** deserialization failures are surfaced as
    /// `Err(RedbCacheError::CacheDeserialization { .. })` and the entry is left in
    /// place. Use this if you prefer to detect data corruption rather than silently
    /// skip it. A strict-mode `remove_expired_entries` sweep aborts on the first
    /// corrupt entry it hits, and because the whole sweep runs in one redb write
    /// transaction, validly-expired entries removed earlier in that same pass are
    /// rolled back too — the store is left exactly as it was before the call.
    ///
    /// Reads and sweeps surface decompression allocation failures and frames above the
    /// configured limit as errors in both modes, preserving the entry.
    ///
    /// In **both** modes, the previous value displaced by `cache_set` is silently
    /// discarded when it cannot be decoded (the write itself succeeds and
    /// `Ok(None)` is returned): the setting governs reads, not the best-effort
    /// previous-value decode on writes.
    #[must_use]
    pub fn strict_deserialization(mut self, strict: bool) -> Self {
        self.strict_deserialization = strict;
        self
    }

    /// Enable compression for new writes. With the `compression_zstd` feature,
    /// reads accept compressed entries even when this setting is omitted.
    #[cfg(feature = "compression_zstd")]
    #[cfg_attr(docsrs, doc(cfg(feature = "compression_zstd")))]
    pub fn compression(mut self, compression: Compression) -> Self {
        self.compression.set_algorithm(compression);
        self
    }

    /// Minimum serialized size in bytes to attempt compression (default: 256).
    /// The store keeps compressed bytes only when they are smaller.
    #[cfg(feature = "compression_zstd")]
    #[cfg_attr(docsrs, doc(cfg(feature = "compression_zstd")))]
    pub fn min_compress_size(mut self, size: usize) -> Self {
        self.compression.set_min_compress_size(size);
        self
    }

    /// Maximum decompressed frame size in bytes (default: 64 MiB).
    /// Larger values are written raw, and this limit does not apply to raw reads.
    /// A compressed entry above this reader's limit returns an error on lookup and stays cached.
    #[cfg(feature = "compression_zstd")]
    #[cfg_attr(docsrs, doc(cfg(feature = "compression_zstd")))]
    pub fn max_decompressed_size(mut self, size: usize) -> Self {
        self.compression.set_max_decompressed_size(size);
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
        // Earlier candidates use the user's XDG cache dir and are preferred; the
        // last is always the temp_dir fallback.
        let candidates = Self::default_disk_dir_candidates();
        let mut last_error = None;

        for disk_dir in candidates {
            match prepare_default_cache_dir(&disk_dir) {
                Ok(()) => return Ok(disk_dir),
                // Fall through to the next candidate when this one is unusable:
                // the filesystem denies writes (a read-only mount aborts with
                // `ReadOnlyFilesystem` rather than `PermissionDenied`, so both
                // must trigger the fallback to temp, DISK-9), or the directory
                // failed the SEC-4 validation and could not be healed.
                Err(error)
                    if matches!(
                        error.kind(),
                        ErrorKind::PermissionDenied | ErrorKind::ReadOnlyFilesystem
                    ) =>
                {
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
    /// - `InvalidCacheName`: `cache_name` is empty, is the component `.` or `..`, or contains
    ///   a character invalid in a filename (a path separator `/` or `\`, one of the
    ///   NTFS/Windows-reserved `:` `<` `>` `"` `|` `?` `*`, or an ASCII control character
    ///   including DEL `0x7F`).
    /// - `Build(BuildError::InvalidValue { field: "ttl", .. })`: the configured TTL is zero.
    /// - `Io`: the cache directory could not be created.
    /// - `Storage`: the redb database file could not be opened or initialized. This includes a
    ///   DAMAGED file (truncated by a full disk, a killed process, an interrupted copy, or a
    ///   restored snapshot): `redb` panics rather than erroring on several kinds of damage, so
    ///   the open runs under [`std::panic::catch_unwind`] and a caught panic is reported as
    ///   `Storage` instead of unwinding out of `build`. The damaged file is left on disk
    ///   untouched — nothing is deleted or recreated — so recovering (or removing) it stays the
    ///   caller's decision. Note this cannot work when the final binary is built with
    ///   `panic = "abort"`, where a `redb` panic still aborts the process.
    pub fn build(self) -> Result<RedbCache<K, V>, RedbCacheBuildError> {
        let cache_name = self
            .cache_name
            .ok_or(super::BuildError::MissingRequired("name"))?;
        // Validate cache_name before using it as a filename component.
        // An empty name yields a meaningless filename. The name is always
        // suffixed with `_v<VERSION>.redb` and used as a cross-platform filename
        // component, so it is restricted to characters that are valid in a
        // filename on every supported platform. A path separator ('/' or '\\')
        // could silently escape the cache directory or create nested
        // subdirectories, and the remaining reserved characters
        // (':' '<' '>' '"' '|' '?' '*' and ASCII control bytes) are invalid in
        // NTFS/Windows filenames. The bare '.' and '..' components are rejected
        // as nonsensical names (and as belt-and-suspenders against traversal,
        // though the version suffix already prevents that). Note ':' is rejected
        // even though module-path-derived names ('a::b') used it: allowing it
        // would make the on-disk file unwritable on Windows.
        {
            let n = &cache_name;
            if n.is_empty()
                || n == "."
                || n == ".."
                || n.chars().any(|c| {
                    matches!(c, '/' | '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*')
                        || c.is_ascii_control()
                })
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
                create_cache_dir(&disk_dir).map_err(RedbCacheBuildError::io)?;
                // An explicit directory's permissions are the caller's choice, but
                // a symlinked directory is still rejected so writes cannot be
                // silently redirected (SEC-4).
                #[cfg(unix)]
                reject_if_symlink(&disk_dir, "cache directory").map_err(RedbCacheBuildError::io)?;
                disk_dir
            }
            None => Self::default_disk_path().map_err(RedbCacheBuildError::io)?,
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
            use std::fs::{OpenOptions, Permissions};
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            // Refuse to follow a symlink planted at the db path (SEC-4).
            reject_if_symlink(&disk_path, "cache db file").map_err(RedbCacheBuildError::io)?;
            let file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .open(&disk_path)
                .map_err(RedbCacheBuildError::io)?;
            // `mode(0o600)` only applies when the file is created. A pre-existing
            // file (e.g. an rc-era db created at 0644) keeps its old permissions,
            // so force 0600 on every open to close the readable-by-group/other
            // window (DISK-7).
            file.set_permissions(Permissions::from_mode(0o600))
                .map_err(RedbCacheBuildError::io)?;
        }

        // Opening a damaged file must be an `Err`, never a process-killing panic:
        // see `open_database`.
        let db = open_database(&disk_path)?;

        Ok(RedbCache {
            ttl: Mutex::new(self.ttl),
            refresh: AtomicBool::new(self.refresh),
            durable: self.durable,
            disk_path,
            connection: Arc::new(db),
            strict_deserialization: self.strict_deserialization,
            compression: self.compression,
            _phantom: self._phantom,
        })
    }
}

/// The `source` recorded on [`RedbCacheBuildError::Storage`] when a `redb` call
/// unwinds instead of returning an error. Private: the concrete source type is
/// explicitly not part of the public API.
#[derive(Debug, Error)]
#[error("{0}")]
struct RedbPanic(String);

/// Extract a human-readable message from a caught panic payload.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> &str {
    if let Some(message) = payload.downcast_ref::<&'static str>() {
        message
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.as_str()
    } else {
        "non-string panic payload"
    }
}

/// Open (or create) the redb database file at `disk_path` and ensure the entry
/// table exists, converting a panic from inside `redb` into a `Storage` error.
///
/// `redb` returns a `DatabaseError` for some kinds of damage but PANICS for
/// others: as of redb 4.1, a file truncated by as little as one byte, or with a
/// flipped byte in the header or an early page, trips an internal assertion
/// (`storage.raw_file_len()? >= header.layout().len()` in its page manager)
/// rather than reporting an error. A truncated tail is the ordinary result of a
/// full disk, a killed container, an interrupted copy, or a restored snapshot,
/// and this file is a DISPOSABLE cache: losing it is a cache miss, so taking the
/// whole application down for it is the wrong response — and [`build`] documents
/// that an unopenable file surfaces as `Storage`. The open therefore runs under
/// [`std::panic::catch_unwind`] and a caught panic becomes
/// [`RedbCacheBuildError::Storage`].
///
/// The damaged file is left exactly as it is on disk. Deleting and recreating it
/// would make `build` succeed, but it would also destroy a file the caller may
/// want to inspect or recover, silently and by default; the caller can remove it
/// (or fall back to an in-memory store) once the error says so.
///
/// The table creation is inside the guarded block on purpose: the first write
/// transaction touches the same damaged structures and can unwind for the same
/// reasons.
///
/// Limitations:
///
/// - `catch_unwind` only catches panics that UNWIND. If the final binary is
///   built with `panic = "abort"`, a `redb` panic still aborts the process and
///   no error can be returned. Nothing in-process can change that.
/// - The panic is still reported by the global panic hook (the usual
///   `thread ... panicked at ...` line on stderr) before it is converted.
///   Silencing it would mean replacing the process-wide hook, which would race
///   with every other thread, so the noise is left alone.
///
/// [`build`]: RedbCacheBuilder::build
fn open_database(disk_path: &Path) -> Result<Database, RedbCacheBuildError> {
    // The closure captures only `&Path` (`Path: RefUnwindSafe`), and nothing
    // observable is left half-modified by a panic here: a partially constructed
    // `Database` is dropped during the unwind, releasing redb's file lock.
    let opened = std::panic::catch_unwind(|| {
        let db = Builder::new()
            .create(disk_path)
            .map_err(RedbCacheBuildError::storage)?;

        // Create the table once at build time so that read transactions always
        // find it (a read txn `open_table` on a never-created table errors with
        // `TableError::TableDoesNotExist`).
        {
            let wtxn = db.begin_write().map_err(RedbCacheBuildError::storage)?;
            wtxn.open_table(TABLE)
                .map_err(RedbCacheBuildError::storage)?;
            wtxn.commit().map_err(RedbCacheBuildError::storage)?;
        }

        Ok(db)
    });

    opened.unwrap_or_else(|payload| {
        Err(RedbCacheBuildError::storage(RedbPanic(format!(
            "redb panicked opening the database file (it is most likely damaged \
             or truncated); the file was left untouched: {}",
            panic_message(payload.as_ref())
        ))))
    })
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

/// Create (if needed) and validate one default-path candidate directory.
///
/// On unix the candidate must pass `validate_cache_dir` (SEC-4: no symlink, not
/// group- or world-writable). A directory created by this crate is always 0700,
/// but a pre-existing one from an earlier `cached` version was created with the
/// process umask — 0775 under the user-private-group scheme common on
/// Debian/Ubuntu (umask 002) — and would fail the validation forever. Since the
/// candidate is an app-derived path (not user-supplied), self-heal that case by
/// tightening the permissions to 0700 and re-validating. The chmod only succeeds
/// for the owner, so an attacker-owned directory still fails validation and the
/// caller falls through to the next candidate.
fn prepare_default_cache_dir(disk_dir: &Path) -> Result<(), std::io::Error> {
    create_cache_dir(disk_dir)?;
    #[cfg(unix)]
    {
        if let Err(error) = validate_cache_dir(disk_dir) {
            // Never chmod through a symlink; that error is not healable here.
            if std::fs::symlink_metadata(disk_dir)?
                .file_type()
                .is_symlink()
            {
                return Err(error);
            }
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(disk_dir, std::fs::Permissions::from_mode(0o700))
                .map_err(|_| error)?;
            validate_cache_dir(disk_dir)?;
        }
    }
    Ok(())
}

/// On unix, validate that a cache directory we resolved on the caller's behalf
/// (an XDG candidate or the temp fallback) is not a symlink and is not group- or
/// world-writable. Guards against symlink-based TOCTOU attacks where an adversary
/// replaces the target directory with a symlink pointing elsewhere.
///
/// Uid ownership is not checked here because obtaining the process uid without
/// a dependency (e.g. `libc` or `rustix`) would require unsafe platform calls.
/// The symlink + permission-bits check is sufficient to reject the most
/// common attack vectors (world-writable directory, symlink redirection).
#[cfg(unix)]
fn validate_cache_dir(path: &Path) -> Result<(), std::io::Error> {
    use std::os::unix::fs::MetadataExt;

    let meta = std::fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        return Err(std::io::Error::new(
            ErrorKind::PermissionDenied,
            "cache directory is a symlink; refusing to use it",
        ));
    }
    // Reject group-writable (0o020) or other-writable (0o002) directories.
    let mode = meta.mode();
    if mode & 0o022 != 0 {
        return Err(std::io::Error::new(
            ErrorKind::PermissionDenied,
            "cache directory has insecure permissions (group- or world-writable)",
        ));
    }
    Ok(())
}

/// On unix, reject a path whose final component is a symlink, so we never follow
/// a planted symlink when creating or opening the db file (or an explicitly
/// configured directory). A not-yet-existing path is allowed: it is about to be
/// created. Unlike `O_NOFOLLOW` this is not atomic, but it needs no `libc`
/// dependency and matches the check `validate_cache_dir` already uses (SEC-4).
#[cfg(unix)]
fn reject_if_symlink(path: &Path, what: &str) -> Result<(), std::io::Error> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(std::io::Error::new(
            ErrorKind::PermissionDenied,
            format!("{what} is a symlink; refusing to use it"),
        )),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Cache store backed by disk, using an embedded [`redb`](https://crates.io/crates/redb)
/// database (one file per cache).
///
/// # TTL
///
/// TTL is **optional**. When no TTL is configured (the default), entries never expire and
/// persist until explicitly removed or the cache is cleared. `RedbCache` tracks expiry
/// client-side via a stored `created_at` timestamp; [`RedisCache`](crate::stores::RedisCache)
/// uses server-side TTL commands and is equally optional. Set a TTL via
/// [`RedbCacheBuilder::ttl`] / [`RedbCacheBuilder::ttl_secs`] / [`RedbCacheBuilder::ttl_millis`]
/// at build time, or update it at runtime with [`ConcurrentCacheTtl::set_ttl`].
///
/// TTL liveness is evaluated against **wall-clock time** (`SystemTime`): redb stores each
/// entry's `created_at` and liveness is recomputed at read time as `now - created_at < ttl`.
/// Unlike the in-memory stores, which use a monotonic clock, this is sensitive to system-clock
/// adjustments. A backward wall-clock jump (an NTP step or a manual clock change) reduces an
/// entry's apparent age and so can extend its effective lifetime past the nominal TTL.
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
///
/// # Format stability
///
/// The on-disk format (the version embedded in the cache file name, the redb table
/// name, and the MessagePack layout of stored values) is stable for the 3.x
/// series: files written by any 3.x release remain readable by every later 3.x
/// release. A format change bumps the embedded version, which isolates old files
/// rather than corrupting them, and is otherwise reserved for a major release.
///
/// Values are encoded with MessagePack in its compact (non-named) form: each
/// entry is a **positional array** of `value` then `created_at`, with the field
/// names absent from the wire. Field order is part of the frozen layout:
/// reordering, inserting or removing a field of the stored value reinterprets
/// every existing entry, so it is a format change and must bump the embedded
/// version, exactly as a type change would.
pub struct RedbCache<K, V> {
    pub(super) ttl: Mutex<Option<Duration>>,
    pub(super) refresh: AtomicBool,
    durable: bool,
    disk_path: PathBuf,
    connection: Arc<Database>,
    strict_deserialization: bool,
    compression: IoValueCompression,
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
    /// Initialize a `RedbCacheBuilder` with the required cache `name`.
    ///
    /// The `name` is used as a cross-platform filename component for the on-disk
    /// database file (see [`RedbCacheBuilder::name`] for the validity rules). It
    /// can be overridden later via [`name`](RedbCacheBuilder::name).
    ///
    /// To construct a builder without supplying the name up front, use
    /// [`RedbCacheBuilder::new`] directly; `build` then returns
    /// `Err(`[`BuildError::MissingRequired`](super::BuildError::MissingRequired)`("name"))`
    /// if the name is never set.
    #[must_use]
    pub fn builder(name: impl Into<String>) -> RedbCacheBuilder<K, V> {
        RedbCacheBuilder::new().name(name)
    }

    /// Return the path of the on-disk redb database file backing this cache.
    #[must_use]
    pub fn disk_path(&self) -> &std::path::Path {
        &self.disk_path
    }

    /// Remove all entries whose TTL has elapsed, returning the number of entries
    /// removed. This is `RedbCache`'s counterpart to the in-memory stores'
    /// [`ConcurrentCacheEvict::evict`](crate::ConcurrentCacheEvict::evict) (which also
    /// returns the removed count); it has its own name and a `Result` return because
    /// sweeping a disk store can fail, so `RedbCache` does not implement that trait.
    ///
    /// In strict-deserialization mode the sweep aborts on the first corrupt entry and
    /// the whole pass is rolled back; see
    /// [`RedbCacheBuilder::strict_deserialization`].
    pub fn remove_expired_entries(&self) -> Result<usize, RedbCacheError> {
        remove_expired_entries_impl::<V>(
            &self.connection,
            *self.ttl.lock(),
            self.strict_deserialization,
            self.durable,
            self.compression,
        )
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

    /// Scan every stored entry and return a [`RedbCacheStats`]: the entry count, how many
    /// are expired or undecodable, and the oldest and newest `created_at`.
    ///
    /// This is an O(n) read of the whole table that decompresses framed entries and
    /// deserializes every stored `V`. It never runs implicitly; [`cache_size`](ConcurrentCacheBase::cache_size) stays
    /// `Ok(None)`. The scan runs under a single read transaction, so it sees one consistent
    /// snapshot and does not block or wait on writers. While it runs, redb cannot reuse pages freed by concurrent
    /// writes, so a long scan under heavy writes can grow the file. It never writes:
    /// undecodable entries are counted, not self-healed, and expired entries are counted,
    /// not removed. In strict-deserialization mode an undecodable entry is still counted
    /// rather than returned as an error. A decompression allocation failure or a frame
    /// above the configured limit aborts the scan with an error.
    ///
    /// `expired` is measured against the TTL current at the call and one `SystemTime::now()`
    /// snapshot. With [`refresh_on_hit`](RedbCacheBuilder::refresh_on_hit) enabled, an
    /// entry's `created_at` is its last write or refresh, so `oldest` / `newest` are the
    /// least / most recently written or refreshed entries.
    pub fn scan_stats(&self) -> Result<RedbCacheStats, RedbCacheError> {
        scan_stats_impl::<V>(&self.connection, *self.ttl.lock(), self.compression)
    }
}

/// Result of [`RedbCache::scan_stats`] (and `async_scan_stats` under the `async` feature).
///
/// `#[non_exhaustive]`: fields may be added in a minor release, so it cannot be built with a
/// struct literal or destructured exhaustively outside the crate. To build one (e.g. in a
/// test), start from [`Default`] and assign the fields.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct RedbCacheStats {
    /// Every stored entry, expired and undecodable included.
    pub entries: usize,
    /// Decodable entries whose age has reached the TTL in effect at the call (the same
    /// `age >= ttl` rule as `cache_get`; TTL changes are retroactive). Always 0 when no TTL
    /// is set.
    pub expired: usize,
    /// Entries whose stored bytes fail to decode (a `cache_get` would self-heal or reject
    /// them).
    pub undecodable: usize,
    /// The earliest `created_at` among decodable entries, expired included.
    pub oldest: Option<SystemTime>,
    /// The latest `created_at` among decodable entries, expired included.
    pub newest: Option<SystemTime>,
}

/// Async counterparts of the blocking `RedbCache` maintenance and inspection methods.
#[cfg(feature = "async")]
#[cfg_attr(docsrs, doc(cfg(feature = "async")))]
impl<K, V> RedbCache<K, V>
where
    V: Serialize + DeserializeOwned + Send + 'static,
{
    /// Async counterpart of [`flush`](RedbCache::flush): runs the durable (fsync)
    /// commit on a background thread (via the [`blocking`] crate) so it does not
    /// stall the async runtime.
    pub async fn async_flush(&self) -> Result<(), RedbCacheError> {
        let connection = self.connection.clone();
        blocking::unblock(move || redb_flush(&connection)).await
    }

    /// Async counterpart of [`remove_expired_entries`](RedbCache::remove_expired_entries):
    /// runs the O(n) TTL sweep on a background thread (via the [`blocking`] crate)
    /// so it does not stall the async runtime. Returns the number of entries removed.
    pub async fn async_remove_expired_entries(&self) -> Result<usize, RedbCacheError> {
        let connection = self.connection.clone();
        let compression = self.compression;
        let ttl = *self.ttl.lock();
        let strict = self.strict_deserialization;
        let durable = self.durable;
        blocking::unblock(move || {
            remove_expired_entries_impl::<V>(&connection, ttl, strict, durable, compression)
        })
        .await
    }

    /// Async counterpart of [`scan_stats`](RedbCache::scan_stats): runs the O(n) scan on a
    /// background thread (via the [`blocking`] crate) so it does not stall the async runtime.
    pub async fn async_scan_stats(&self) -> Result<RedbCacheStats, RedbCacheError> {
        let connection = self.connection.clone();
        let compression = self.compression;
        let ttl = *self.ttl.lock();
        blocking::unblock(move || scan_stats_impl::<V>(&connection, ttl, compression)).await
    }
}

#[non_exhaustive]
#[derive(Error)]
pub enum RedbCacheError {
    /// A redb storage or transaction error.
    ///
    /// **Semver note:** the concrete source type is NOT part of the public API
    /// and may change without a major version bump. Inspect via
    /// [`std::error::Error::source`] and downcast if needed.
    #[error("storage error: {source}")]
    Storage {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
    /// A stored value failed to deserialize.
    ///
    /// **Security note:** `cached_value` contains the raw bytes that were read
    /// from disk and failed to decode. Those bytes may contain sensitive
    /// application data. Do not log or display this error variant without
    /// redacting or omitting the `cached_value` field.
    ///
    /// **Semver note:** the concrete source type is NOT part of the public API.
    #[error("error deserializing cached value: {source}")]
    CacheDeserialization {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
        cached_value: Vec<u8>,
    },
    /// A value failed to serialize before writing.
    ///
    /// **Semver note:** the concrete source type is NOT part of the public API.
    #[error("error serializing cached value: {source}")]
    CacheSerialization {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
}

// Manual `Debug` that redacts the raw `cached_value` bytes (they may carry
// sensitive application data read from disk) as `<N bytes redacted>`, so a `{:?}`
// of the error never leaks the payload.
impl std::fmt::Debug for RedbCacheError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Storage { source } => f.debug_struct("Storage").field("source", source).finish(),
            Self::CacheDeserialization {
                source,
                cached_value,
            } => f
                .debug_struct("CacheDeserialization")
                .field("source", source)
                .field(
                    "cached_value",
                    &format_args!("<{} bytes redacted>", cached_value.len()),
                )
                .finish(),
            Self::CacheSerialization { source } => f
                .debug_struct("CacheSerialization")
                .field("source", source)
                .finish(),
        }
    }
}

impl RedbCacheError {
    pub(crate) fn storage(e: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Storage {
            source: Box::new(e),
        }
    }
    pub(crate) fn serialization(e: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::CacheSerialization {
            source: Box::new(e),
        }
    }
    pub(crate) fn deserialization(
        e: impl Into<Box<dyn std::error::Error + Send + Sync + 'static>>,
        cached_value: Vec<u8>,
    ) -> Self {
        Self::CacheDeserialization {
            source: e.into(),
            cached_value,
        }
    }
    /// Returns `true` if this error is a deserialization failure.
    #[must_use]
    pub fn is_deserialization(&self) -> bool {
        matches!(self, Self::CacheDeserialization { .. })
    }
}

/// Stored entry. Serialized with `rmp_serde::to_vec` (never `to_vec_named`), so
/// the on-disk form is a 2-element positional MessagePack array of `value` then
/// `created_at`. Field order is frozen for 3.x: see `DISK_FILE_VERSION`.
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
/// `CachedDiskValue::new(value)`: the encoding is positional, so the two structs
/// must keep the same fields in the same order for values written through either
/// path to deserialize identically.
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

fn serialize_disk_value(
    value: &impl Serialize,
    compression: IoValueCompression,
) -> Result<Vec<u8>, RedbCacheError> {
    let bytes = rmp_serde::to_vec(value).map_err(RedbCacheError::serialization)?;
    compress_disk_value(bytes, compression)
}

fn compress_disk_value(
    bytes: Vec<u8>,
    compression: IoValueCompression,
) -> Result<Vec<u8>, RedbCacheError> {
    compression
        .encode(bytes)
        .map_err(RedbCacheError::serialization)
}

fn deserialize_disk_value<V: DeserializeOwned>(
    bytes: &[u8],
    compression: IoValueCompression,
) -> Result<CachedDiskValue<V>, Box<dyn std::error::Error + Send + Sync + 'static>> {
    let decoded = compression.decode(bytes)?;
    Ok(rmp_serde::from_slice(&decoded)?)
}

/// Begin a write txn with the durability implied by `durable`.
/// Durability is set on the fresh transaction (it only needs to be set before the
/// eventual `commit`); callers then open the table and commit.
fn begin_write(
    connection: &Database,
    durable: bool,
) -> Result<redb::WriteTransaction, RedbCacheError> {
    let mut wtxn = connection.begin_write().map_err(RedbCacheError::storage)?;
    if !durable {
        wtxn.set_durability(Durability::None)
            .map_err(RedbCacheError::storage)?;
    }
    Ok(wtxn)
}

fn disk_cache_get<V>(
    connection: &Database,
    key: &str,
    ttl: Option<Duration>,
    refresh: bool,
    durable: bool,
    strict: bool,
    compression: IoValueCompression,
) -> Result<Option<V>, RedbCacheError>
where
    V: Serialize + DeserializeOwned,
{
    // Fast path: read the entry under a read transaction. For the common case
    // where no mutation is required (no TTL, or a fresh entry with refresh
    // disabled) we return immediately, keeping a single read txn with no write.
    let raw_bytes: Vec<u8> = {
        let rtxn = connection.begin_read().map_err(RedbCacheError::storage)?;
        let table = rtxn.open_table(TABLE).map_err(RedbCacheError::storage)?;
        let Some(guard) = table.get(key).map_err(RedbCacheError::storage)? else {
            return Ok(None);
        };
        // Clone bytes before the guard/table/txn are dropped.
        guard.value().to_vec()
    };

    let cached: CachedDiskValue<V> = match deserialize_disk_value::<V>(&raw_bytes, compression) {
        Ok(v) => v,
        Err(e) if !strict && !should_preserve_on_decode_error(e.as_ref()) => {
            // D2/C5: self-heal. The entry read under the (now-dropped) read txn
            // failed to decode. Do NOT blindly delete it: a concurrent
            // `cache_set` may have committed a valid value between our read txn
            // and now, and a blind remove would silently drop that fresh write.
            //
            // Instead re-read the entry under the write txn and re-validate.
            // redb serialises write transactions, so this check-and-mutate is
            // atomic against any concurrent writer (mirrors the refresh/expiry
            // re-read at the mutation path below). Only delete if the entry is
            // *still* corrupt; if it now decodes, adopt the fresh value and fall
            // through to the normal TTL handling.
            let wtxn = begin_write(connection, durable)?;
            let healed: Result<Option<CachedDiskValue<V>>, RedbCacheError> = {
                let mut table = wtxn.open_table(TABLE).map_err(RedbCacheError::storage)?;
                // guard is dropped after .map(); table can be mutated below.
                let current_bytes: Option<Vec<u8>> = table
                    .get(key)
                    .map_err(RedbCacheError::storage)?
                    .map(|guard| guard.value().to_vec());
                match current_bytes {
                    // Entry vanished (concurrent remove/clear): nothing to heal.
                    None => Ok(None),
                    Some(bytes) => match deserialize_disk_value::<V>(&bytes, compression) {
                        // A concurrent writer stored a valid value; keep it.
                        Ok(v) => Ok(Some(v)),
                        // Still corrupt under the write txn: evict it.
                        Err(e) if !should_preserve_on_decode_error(e.as_ref()) => {
                            table.remove(key).map_err(RedbCacheError::storage)?;
                            Ok(None)
                        }
                        Err(e) => Err(RedbCacheError::deserialization(e, bytes)),
                    },
                }
            };
            let healed = healed?;
            wtxn.commit().map_err(RedbCacheError::storage)?;
            match healed {
                None => return Ok(None),
                Some(v) => v,
            }
        }
        Err(e) => return Err(RedbCacheError::deserialization(e, raw_bytes)),
    };

    let Some(ttl) = ttl else {
        // No TTL: entries never expire; no mutation needed.
        return Ok(Some(cached.value));
    };

    let age = SystemTime::now()
        .duration_since(cached.created_at)
        .unwrap_or(Duration::from_secs(0));

    if age < ttl && !refresh {
        // Entry is fresh and no refresh is requested: fast path, no write.
        return Ok(Some(cached.value));
    }

    // Mutation is required — either a refresh-on-hit write-back or an expiry
    // eviction. To avoid two read-then-write races, we open a write transaction
    // and re-read the entry atomically:
    //
    // Race 1 – refresh clobber: the naive path serialises the old value (read
    //   under the now-dropped read txn) with a refreshed timestamp and blindly
    //   inserts it, overwriting a newer value a concurrent `cache_set` may have
    //   stored in between.
    //
    // Race 2 – stale eviction: the naive path removes the entry in a new write
    //   txn, deleting a fresh value a concurrent `cache_set` may have just
    //   written in between.
    //
    // redb serialises write transactions, so the re-read + conditional mutate
    // below is atomic against any concurrent writer.
    let wtxn = begin_write(connection, durable)?;
    // Use a Result<Option<V>> block so errors propagate and table is dropped
    // before commit (redb requires WriteTransaction to outlive open Tables).
    let result: Result<Option<V>, RedbCacheError> = {
        let mut table = wtxn.open_table(TABLE).map_err(RedbCacheError::storage)?;

        // Re-read the current entry under the write txn. Drop the access guard
        // guard is dropped after .map(); table can be mutated below.
        let current_bytes: Option<Vec<u8>> = table
            .get(key)
            .map_err(RedbCacheError::storage)?
            .map(|guard| guard.value().to_vec());

        match current_bytes {
            None => {
                // Entry vanished between the read txn and this write txn (a
                // concurrent remove or clear). Do not resurrect it.
                Ok(None)
            }
            Some(bytes) => {
                let current: CachedDiskValue<V> =
                    match deserialize_disk_value::<V>(&bytes, compression) {
                        Ok(v) => v,
                        Err(e) if !strict && !should_preserve_on_decode_error(e.as_ref()) => {
                            // D2: corrupt under write txn — evict it. Table is
                            // dropped at end of this block; commit follows.
                            table.remove(key).map_err(RedbCacheError::storage)?;
                            return {
                                // table is dropped; safe to consume wtxn.
                                drop(table);
                                wtxn.commit().map_err(RedbCacheError::storage)?;
                                Ok(None)
                            };
                        }
                        Err(e) => return Err(RedbCacheError::deserialization(e, bytes)),
                    };

                let current_age = SystemTime::now()
                    .duration_since(current.created_at)
                    .unwrap_or(Duration::from_secs(0));

                if current_age < ttl {
                    // Entry is still fresh under the write txn. The current
                    // value is authoritative — it may differ from what the read
                    // txn saw if a concurrent `cache_set` replaced it. Refresh
                    // the current entry (not the stale read) if requested.
                    if refresh {
                        let mut refreshed = current;
                        refreshed.refresh_created_at();
                        let serialized = serialize_disk_value(&refreshed, compression)?;
                        table
                            .insert(key, serialized.as_slice())
                            .map_err(RedbCacheError::storage)?;
                        Ok(Some(refreshed.value))
                    } else {
                        Ok(Some(current.value))
                    }
                } else {
                    // Still expired under the write txn: remove it.
                    table.remove(key).map_err(RedbCacheError::storage)?;
                    Ok(None)
                }
            }
        }
        // table is dropped here
    };
    wtxn.commit().map_err(RedbCacheError::storage)?;
    result
}

fn disk_cache_set<V>(
    connection: &Database,
    key: &str,
    serialized: Vec<u8>,
    ttl: Option<Duration>,
    durable: bool,
    compression: IoValueCompression,
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
        let mut table = wtxn.open_table(TABLE).map_err(RedbCacheError::storage)?;
        table
            .insert(key, serialized.as_slice())
            .map_err(RedbCacheError::storage)?
            .map(|guard| guard.value().to_vec())
    };
    wtxn.commit().map_err(RedbCacheError::storage)?;
    // `cache_set` returns the previous LIVE value only: a displaced entry that
    // was already past the TTL is reported as `None`, matching the
    // `ConcurrentCached::cache_set` contract and the in-memory/redis stores
    // (where an expired key is simply not there any more). The write above
    // already overwrote it, so the stale entry is gone from disk either way;
    // this filter only decides what the caller is handed back. Same shape as
    // `disk_cache_remove`, which filters the removed entry identically.
    Ok(previous_bytes
        .and_then(|bytes| deserialize_disk_value::<V>(&bytes, compression).ok())
        .filter(|cached| {
            ttl.is_none_or(|ttl| {
                SystemTime::now()
                    .duration_since(cached.created_at)
                    .unwrap_or(Duration::from_secs(0))
                    < ttl
            })
        })
        .map(|cached| cached.value))
}

/// Insert `serialized` under `key` without reading back the displaced value.
/// Backs [`SerializeCached::cache_set_ref`](crate::SerializeCached::cache_set_ref):
/// the previous `AccessGuard` returned by `insert` is dropped without being
/// materialized or decoded, saving a byte copy and a deserialize on every write.
fn disk_cache_set_no_return(
    connection: &Database,
    key: &str,
    serialized: Vec<u8>,
    durable: bool,
) -> Result<(), RedbCacheError> {
    let wtxn = begin_write(connection, durable)?;
    {
        let mut table = wtxn.open_table(TABLE).map_err(RedbCacheError::storage)?;
        table
            .insert(key, serialized.as_slice())
            .map_err(RedbCacheError::storage)?;
    }
    wtxn.commit().map_err(RedbCacheError::storage)?;
    Ok(())
}

fn disk_cache_remove<V>(
    connection: &Database,
    key: &str,
    ttl: Option<Duration>,
    durable: bool,
    compression: IoValueCompression,
) -> Result<Option<V>, RedbCacheError>
where
    V: DeserializeOwned,
{
    let wtxn = begin_write(connection, durable)?;
    // Copy the removed bytes (owned) and commit before deserializing, so the entry
    // is removed regardless of whether its value can be decoded. The removal
    // succeeded, so an undecodable value is reported as `None` rather than an error.
    let removed_bytes: Option<Vec<u8>> = {
        let mut table = wtxn.open_table(TABLE).map_err(RedbCacheError::storage)?;
        table
            .remove(key)
            .map_err(RedbCacheError::storage)?
            .map(|guard| guard.value().to_vec())
    };
    wtxn.commit().map_err(RedbCacheError::storage)?;

    let removed =
        removed_bytes.and_then(|bytes| deserialize_disk_value::<V>(&bytes, compression).ok());
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
    compression: IoValueCompression,
) -> Result<Option<V>, RedbCacheError>
where
    V: DeserializeOwned,
{
    let wtxn = begin_write(connection, durable)?;
    // Copy the removed bytes (owned) and commit before deserializing, so the entry
    // is removed regardless of whether its value can be decoded. The removal
    // succeeded, so an undecodable value is reported as `None` rather than an error.
    let removed_bytes: Option<Vec<u8>> = {
        let mut table = wtxn.open_table(TABLE).map_err(RedbCacheError::storage)?;
        table
            .remove(key)
            .map_err(RedbCacheError::storage)?
            .map(|guard| guard.value().to_vec())
    };
    wtxn.commit().map_err(RedbCacheError::storage)?;
    Ok(removed_bytes
        .and_then(|bytes| deserialize_disk_value::<V>(&bytes, compression).ok())
        .map(|cached| cached.value))
}

fn disk_cache_delete(
    connection: &Database,
    key: &str,
    durable: bool,
) -> Result<bool, RedbCacheError> {
    let wtxn = begin_write(connection, durable)?;
    let removed = {
        let mut table = wtxn.open_table(TABLE).map_err(RedbCacheError::storage)?;
        table
            .remove(key)
            .map_err(RedbCacheError::storage)?
            .is_some()
    };
    wtxn.commit().map_err(RedbCacheError::storage)?;
    Ok(removed)
}

/// Remove every entry from the cache table. Drops and recreates the table in a
/// single write txn so subsequent read txns still find an (empty) table rather
/// than erroring with `TableError::TableDoesNotExist`.
fn disk_cache_clear(connection: &Database, durable: bool) -> Result<(), RedbCacheError> {
    let wtxn = begin_write(connection, durable)?;
    wtxn.delete_table(TABLE).map_err(RedbCacheError::storage)?;
    wtxn.open_table(TABLE).map_err(RedbCacheError::storage)?;
    wtxn.commit().map_err(RedbCacheError::storage)?;
    Ok(())
}

/// Force a durable (fsync) commit. An empty write transaction committed with
/// [`Durability::Immediate`] persists everything written so far, including prior
/// `Durability::None` commits (the writes made while `durable`
/// is `false`).
fn redb_flush(connection: &Database) -> Result<(), RedbCacheError> {
    let mut wtxn = connection.begin_write().map_err(RedbCacheError::storage)?;
    wtxn.set_durability(Durability::Immediate)
        .map_err(RedbCacheError::storage)?;
    wtxn.commit().map_err(RedbCacheError::storage)?;
    Ok(())
}

/// Shared implementation of [`RedbCache::remove_expired_entries`]. Kept as a free
/// function so both the synchronous method and the async
/// [`RedbCache::async_remove_expired_entries`] (which runs it via
/// [`blocking::unblock`]) share one source of truth for the sweep behavior.
fn remove_expired_entries_impl<V>(
    connection: &Database,
    ttl: Option<Duration>,
    strict: bool,
    durable: bool,
    compression: IoValueCompression,
) -> Result<usize, RedbCacheError>
where
    V: Serialize + DeserializeOwned,
{
    // DISK-5: if no TTL is configured, there are no TTL-based expirations.
    let Some(ttl) = ttl else {
        return Ok(0);
    };

    // Take a single time snapshot for both the scan and write passes (B3: two separate
    // `SystemTime::now()` calls could yield different instants, causing an entry to be
    // judged unexpired in the scan and expired in the write or vice-versa).
    let now = SystemTime::now();

    // Collect candidate expired keys under a read transaction. We cannot
    // iterate and remove entries in the same transaction because the
    // iterator borrows the read txn for its entire lifetime.
    let mut expired_keys: Vec<String> = Vec::new();
    {
        let rtxn = connection.begin_read().map_err(RedbCacheError::storage)?;
        let table = rtxn.open_table(TABLE).map_err(RedbCacheError::storage)?;
        for item in table.iter().map_err(RedbCacheError::storage)? {
            let (key, value) = item.map_err(RedbCacheError::storage)?;
            let raw = value.value();
            match deserialize_disk_value::<V>(raw, compression) {
                Ok(cached) => {
                    if now
                        .duration_since(cached.created_at)
                        .unwrap_or(Duration::from_secs(0))
                        >= ttl
                    {
                        expired_keys.push(key.value().to_string());
                    }
                }
                Err(e) if !strict && !should_preserve_on_decode_error(e.as_ref()) => {
                    // D2: undecodable entry is treated as an eviction candidate.
                    expired_keys.push(key.value().to_string());
                }
                Err(e) => {
                    return Err(RedbCacheError::deserialization(e, raw.to_vec()));
                }
            }
        }
    }

    if expired_keys.is_empty() {
        return Ok(0);
    }

    // Re-check each candidate key inside the write txn before removing it.
    // A concurrent `cache_set` may have replaced one of these entries with a
    // fresh value between the read scan above and now; skipping such entries
    // ensures we never delete a live entry. The returned count reflects the
    // number of entries *actually* removed, not the number found in the scan.
    let mut removed = 0usize;
    let wtxn = begin_write(connection, durable)?;
    {
        let mut table = wtxn.open_table(TABLE).map_err(RedbCacheError::storage)?;
        for key in &expired_keys {
            // Re-read the entry under the write txn to obtain the
            // authoritative state. Drop the access guard before mutating
            // the table.
            // guard is dropped after .map(); table can be mutated below.
            let raw_bytes: Option<Vec<u8>> = table
                .get(key.as_str())
                .map_err(RedbCacheError::storage)?
                .map(|guard| guard.value().to_vec());

            match raw_bytes {
                None => {
                    // Already removed by a concurrent operation; skip.
                }
                Some(bytes) => {
                    match deserialize_disk_value::<V>(&bytes, compression) {
                        Ok(entry) => {
                            if now
                                .duration_since(entry.created_at)
                                .unwrap_or(Duration::from_secs(0))
                                >= ttl
                            {
                                table
                                    .remove(key.as_str())
                                    .map_err(RedbCacheError::storage)?;
                                removed += 1;
                            }
                            // else: entry was rewritten with a fresh timestamp by a
                            // concurrent writer; leave it in place.
                        }
                        Err(e) if !strict && !should_preserve_on_decode_error(e.as_ref()) => {
                            // D2: corrupt under write txn too — evict it.
                            table
                                .remove(key.as_str())
                                .map_err(RedbCacheError::storage)?;
                            removed += 1;
                        }
                        Err(e) => {
                            return Err(RedbCacheError::deserialization(e, bytes));
                        }
                    }
                }
            }
        }
    }
    wtxn.commit().map_err(RedbCacheError::storage)?;
    Ok(removed)
}

/// Shared implementation of [`RedbCache::scan_stats`] and the async
/// `RedbCache::async_scan_stats`. Read-only: one read txn, no write txn, so a
/// corrupt or expired entry is only counted, never healed or removed.
fn scan_stats_impl<V>(
    connection: &Database,
    ttl: Option<Duration>,
    compression: IoValueCompression,
) -> Result<RedbCacheStats, RedbCacheError>
where
    V: DeserializeOwned,
{
    // One time snapshot for the whole scan, same rule as `remove_expired_entries_impl`.
    let now = SystemTime::now();
    let mut stats = RedbCacheStats::default();

    let rtxn = connection.begin_read().map_err(RedbCacheError::storage)?;
    let table = rtxn.open_table(TABLE).map_err(RedbCacheError::storage)?;
    for item in table.iter().map_err(RedbCacheError::storage)? {
        let (_, value) = item.map_err(RedbCacheError::storage)?;
        stats.entries += 1;
        // Decode the whole entry, `V` included, so `undecodable` matches what
        // `cache_get` would self-heal or reject.
        let raw = value.value();
        let cached = match deserialize_disk_value::<V>(raw, compression) {
            Ok(cached) => cached,
            Err(e) if should_preserve_on_decode_error(e.as_ref()) => {
                return Err(RedbCacheError::deserialization(e, raw.to_vec()));
            }
            Err(_) => {
                stats.undecodable += 1;
                continue;
            }
        };
        if ttl.is_some_and(|ttl| {
            now.duration_since(cached.created_at)
                .unwrap_or(Duration::from_secs(0))
                >= ttl
        }) {
            stats.expired += 1;
        }
        let created_at = cached.created_at;
        stats.oldest = Some(stats.oldest.map_or(created_at, |t| t.min(created_at)));
        stats.newest = Some(stats.newest.map_or(created_at, |t| t.max(created_at)));
    }
    Ok(stats)
}

/// Behavior on a corrupt stored value (one whose bytes fail to deserialize):
///
/// In the default mode (`strict_deserialization = false`), `cache_get` self-heals: the
/// undecodable entry is deleted and `Ok(None)` is returned (a transparent miss). In strict
/// mode (`strict_deserialization = true`), `cache_get` and `remove_expired_entries` surface
/// [`RedbCacheError::CacheDeserialization`] instead. In both modes, `cache_set`,
/// `cache_remove`, and `cache_remove_entry` succeed — they write/remove the entry regardless
/// and report the undecodable previous value as `Ok(None)` (a write that took effect is never
/// reported as an error). The same holds for the `ConcurrentCachedAsync` impl.
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

    /// Set the TTL applied when computing entry liveness, returning the previous TTL
    /// (`None` if expiry was disabled).
    ///
    /// The change is retroactive and applies to existing entries, not just future writes.
    /// redb persists only each entry's `created_at` timestamp, never an absolute per-entry
    /// expiry, so liveness is recomputed at read time as `now - created_at < ttl` against
    /// whatever TTL is current. Lowering the TTL can immediately expire already-stored
    /// entries; raising it can revive entries a shorter TTL would have expired.
    ///
    /// A zero `ttl` disables expiry, exactly equivalent to `unset_ttl`: with no TTL, every
    /// stored entry is considered live regardless of age.
    fn set_ttl(&self, ttl: Duration) -> Option<Duration> {
        let mut guard = self.ttl.lock();
        if ttl.is_zero() {
            guard.take()
        } else {
            guard.replace(ttl)
        }
    }

    /// Disable expiry, returning the previous TTL (`None` if it was already disabled).
    ///
    /// This is retroactive: because liveness is recomputed at read time from each entry's
    /// stored `created_at`, disabling the TTL makes every stored entry live regardless of
    /// age, including entries the prior TTL would have expired.
    fn unset_ttl(&self) -> Option<Duration> {
        self.ttl.lock().take()
    }
}

impl<K, V> ConcurrentCacheRefreshOnHit for RedbCache<K, V> {
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
            self.strict_deserialization,
            self.compression,
        )
    }

    /// Insert `key`/`value`, returning the previous **live** value at that key.
    ///
    /// A displaced entry that was already past the TTL is filtered to `None`
    /// (per the [`ConcurrentCached::cache_set`] contract); it is overwritten on
    /// disk either way.
    fn cache_set(&self, key: K, value: V) -> Result<Option<V>, RedbCacheError> {
        let ttl = *self.ttl.lock();
        let serialized = serialize_disk_value(&CachedDiskValue::new(value), self.compression)?;
        disk_cache_set(
            &self.connection,
            &key.to_string(),
            serialized,
            ttl,
            self.durable,
            self.compression,
        )
    }

    fn cache_remove(&self, key: &K) -> Result<Option<V>, RedbCacheError> {
        let ttl = *self.ttl.lock();
        disk_cache_remove(
            &self.connection,
            &key.to_string(),
            ttl,
            self.durable,
            self.compression,
        )
    }

    fn cache_remove_entry(&self, key: &K) -> Result<Option<(K, V)>, Self::Error> {
        disk_cache_remove_entry(
            &self.connection,
            &key.to_string(),
            self.durable,
            self.compression,
        )
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
    ///
    /// Note: clearing empties the table but does not shrink the on-disk file. redb does not
    /// return freed pages to the filesystem, and compaction is not currently exposed, so the
    /// file keeps its high-water-mark size and reuses that space for later writes.
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

    /// Returns `true` if the cache contains a non-expired entry for `key`.
    ///
    /// Delegates to [`cache_get`](ConcurrentCached::cache_get): the value is
    /// fetched and deserialized to determine presence.
    ///
    /// **This is not a read-only probe.** Because it is a real `cache_get`, it
    /// takes the same paths that call has: with
    /// [`refresh_on_hit`](ConcurrentCacheRefreshOnHit) enabled a presence check
    /// RENEWS the entry's TTL, and it costs a write transaction (plus an fsync
    /// when built with `durable(true)`) rather than a read transaction.
    /// Expired-entry eviction and non-strict self-heal likewise write. Same
    /// behavior as `RedisCache`. If you need a probe that neither renews nor
    /// writes, disable `refresh_on_hit` (see
    /// [`set_refresh_on_hit`](ConcurrentCacheRefreshOnHit::set_refresh_on_hit))
    /// for the duration of the check.
    fn cache_contains(&self, k: &K) -> Result<bool, Self::Error> {
        self.cache_get(k).map(|v| v.is_some())
    }
}

impl<K, V> crate::SerializeCached<K, V> for RedbCache<K, V>
where
    K: ToString + Clone,
    V: Serialize + DeserializeOwned,
{
    /// Serializes from the borrowed `value` (no clone) and writes it under
    /// `key.to_string()`. Equivalent to [`ConcurrentCached::cache_set`] but avoids
    /// taking ownership of `value` and does not read back the previous value.
    /// Call [`ConcurrentCached::cache_get`] first if you need the prior value.
    fn cache_set_ref(&self, key: &K, value: &V) -> Result<(), RedbCacheError> {
        let serialized = serialize_disk_value(&CachedDiskValueRef::new(value), self.compression)?;
        disk_cache_set_no_return(&self.connection, &key.to_string(), serialized, self.durable)
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
/// (only the owned serialized bytes). Compression runs on the blocking pool.
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
        let compression = self.compression;
        let key = key.to_string();
        let (ttl, refresh, durable, strict) = (
            *self.ttl.lock(),
            self.refresh.load(Ordering::Relaxed),
            self.durable,
            self.strict_deserialization,
        );
        blocking::unblock(move || {
            disk_cache_get::<V>(
                &connection,
                &key,
                ttl,
                refresh,
                durable,
                strict,
                compression,
            )
        })
        .await
    }

    /// Async counterpart of [`ConcurrentCached::cache_set`]: returns the previous
    /// **live** value, filtering an already-expired displaced entry to `None`.
    async fn async_cache_set(&self, key: K, value: V) -> Result<Option<V>, RedbCacheError> {
        let connection = self.connection.clone();
        let compression = self.compression;
        let key = key.to_string();
        let (ttl, durable) = (*self.ttl.lock(), self.durable);
        let serialized = rmp_serde::to_vec(&CachedDiskValue::new(value))
            .map_err(RedbCacheError::serialization)?;
        blocking::unblock(move || {
            let serialized = compress_disk_value(serialized, compression)?;
            disk_cache_set::<V>(&connection, &key, serialized, ttl, durable, compression)
        })
        .await
    }

    async fn async_cache_remove(&self, key: &K) -> Result<Option<V>, RedbCacheError> {
        let connection = self.connection.clone();
        let compression = self.compression;
        let key = key.to_string();
        let (ttl, durable) = (*self.ttl.lock(), self.durable);
        blocking::unblock(move || {
            disk_cache_remove::<V>(&connection, &key, ttl, durable, compression)
        })
        .await
    }

    async fn async_cache_remove_entry(&self, key: &K) -> Result<Option<(K, V)>, Self::Error> {
        let connection = self.connection.clone();
        let compression = self.compression;
        let key_str = key.to_string();
        let durable = self.durable;
        let v: Option<V> = blocking::unblock(move || {
            disk_cache_remove_entry::<V>(&connection, &key_str, durable, compression)
        })
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

    /// Returns `true` if the cache contains a non-expired entry for `key`.
    ///
    /// Delegates to [`async_cache_get`](crate::ConcurrentCachedAsync::async_cache_get)
    /// via a background thread (same pattern as all other async methods here).
    async fn async_cache_contains(&self, k: &K) -> Result<bool, RedbCacheError>
    where
        Self: Sized + Sync,
        K: Sync,
    {
        self.async_cache_get(k).await.map(|v| v.is_some())
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
    /// Does not read back the previous value.
    ///
    /// Serialization happens eagerly (before the returned future is awaited) so the
    /// borrowed `&V` is never held across the `.await`. This keeps the `V: Send`
    /// (not `Sync`) bound consistent with `async_cache_set`.
    fn async_cache_set_ref(
        &self,
        key: &K,
        value: &V,
    ) -> impl std::future::Future<Output = Result<(), RedbCacheError>> + Send {
        let connection = self.connection.clone();
        let key = key.to_string();
        let durable = self.durable;
        let compression = self.compression;
        // Serialize eagerly; defer any error into the future.
        let serialized = rmp_serde::to_vec(&CachedDiskValueRef::new(value))
            .map_err(RedbCacheError::serialization);
        async move {
            let serialized = serialized?;
            blocking::unblock(move || {
                let serialized = compress_disk_value(serialized, compression)?;
                disk_cache_set_no_return(&connection, &key, serialized, durable)
            })
            .await
        }
    }
}

#[cfg(test)]
mod tests;
