//! A damaged redb backing file must surface as `Err`, never as a panic.
//!
//! `RedbCacheBuilder::build` documents that a database file it cannot open is
//! reported as `RedbCacheBuildError::Storage`. It did not hold: `redb` returns a
//! `DatabaseError` for some damage but PANICS for the rest. As of redb 4.1 a
//! file truncated by a single byte, or with a flipped byte in the header or an
//! early page, trips an internal assertion
//! (`storage.raw_file_len()? >= header.layout().len()` in its page manager) and
//! unwinds straight out of `build`.
//!
//! That matters because a truncated tail is the ORDINARY result of a full disk,
//! a killed container, an interrupted copy, or a restored snapshot — and this
//! file is a disposable cache. Losing it is a cache miss; taking the whole
//! application down for it is not an acceptable failure mode, and a caller
//! cannot even defend itself with `match` because the failure never becomes a
//! value.
//!
//! Every build below runs inside `catch_unwind` so a regression fails an
//! assertion instead of aborting the whole test binary (and so the pre-fix
//! failure is legible rather than a dead harness). Note `catch_unwind` — here
//! and in the fix it verifies — cannot help a binary built with
//! `panic = "abort"`; that limitation is documented on the store.
//!
//! Gated on `redb_store` like every other redb test. Run with
//! `cargo test --features redb_store`.

#![cfg(feature = "redb_store")]

use std::any::Any;
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use cached::{ConcurrentCached, RedbCache, RedbCacheBuildError};
use tempfile::TempDir;

/// Scratch databases live in the repo's gitignored `local/` directory rather
/// than the system temp dir. `TempDir` still removes the directory on drop, so
/// nothing is left behind.
fn scratch_dir() -> TempDir {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("local");
    std::fs::create_dir_all(&root).expect("create local/ scratch root");
    TempDir::new_in(root).expect("create scratch dir")
}

/// Build a real, healthy cache file with some content in it, then close it and
/// return its path. redb takes an exclusive file lock and finalizes on drop, so
/// the handle must be gone before the file is damaged or reopened.
fn make_healthy_db(dir: &TempDir, name: &str) -> PathBuf {
    let cache: RedbCache<u32, u32> = RedbCache::builder(name)
        .disk_dir(dir.path())
        // Durable so the entries are fully materialized in the file being damaged.
        .durable(true)
        .build()
        .expect("healthy build");
    for k in 0..64u32 {
        cache.cache_set(k, k * 10).unwrap();
    }
    let path = cache.disk_path().to_path_buf();
    drop(cache);
    assert!(path.is_file(), "healthy db file must exist at {path:?}");
    path
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).expect("stat db file").len()
}

/// Shorten the file by `bytes` — the shape of damage a full disk, a killed
/// process, or an interrupted copy actually leaves behind.
fn truncate_by(path: &Path, bytes: u64) {
    let len = file_len(path);
    assert!(
        len > bytes,
        "db file ({len} bytes) is too small to truncate by {bytes}"
    );
    let file = OpenOptions::new()
        .write(true)
        .open(path)
        .expect("open for truncate");
    file.set_len(len - bytes).expect("truncate db file");
}

/// Invert the byte at `offset` — the shape of damage bit rot or a partial page
/// write leaves behind.
fn flip_byte(path: &Path, offset: u64) {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("open for byte flip");
    file.seek(SeekFrom::Start(offset)).expect("seek");
    let mut byte = [0u8; 1];
    file.read_exact(&mut byte).expect("read byte");
    file.seek(SeekFrom::Start(offset)).expect("seek back");
    file.write_all(&[!byte[0]]).expect("write flipped byte");
    file.sync_all().expect("sync flipped byte");
}

/// Attempt a build without letting a panic escape.
///
/// `Ok(_)` means `build` RETURNED (success or error, both fine — it produced a
/// value the caller can handle). `Err(_)` means it unwound, which is the bug.
#[allow(clippy::type_complexity)]
fn build_catching_panic(
    dir: &TempDir,
    name: &str,
) -> Result<Result<RedbCache<u32, u32>, RedbCacheBuildError>, Box<dyn Any + Send>> {
    let disk_dir = dir.path().to_path_buf();
    let name = name.to_string();
    std::panic::catch_unwind(move || {
        RedbCache::<u32, u32>::builder(name.as_str())
            .disk_dir(&disk_dir)
            .durable(false)
            .build()
    })
}

/// The core contract, applied to every damage fixture: `build` must RETURN. If
/// it reports a failure that failure must be `Storage` (never another variant),
/// and the damaged file must be left exactly as it was found.
///
/// Whether a given byte of damage is fatal at all is redb's business — a flipped
/// byte in a region the open path never reads is legitimately openable — so this
/// helper does not demand an error, only that any failure arrives as a value.
/// Returns the rendered error when there was one.
fn assert_no_panic(dir: &TempDir, name: &str, path: &Path, damage: &str) -> Option<String> {
    let len_before = file_len(path);
    let outcome = build_catching_panic(dir, name);

    let result = match outcome {
        Ok(result) => result,
        Err(_) => panic!(
            "build() PANICKED on a damaged db file ({damage}) instead of returning \
             Err(Storage); a torn cache file must be a recoverable error, not a \
             process-killing unwind"
        ),
    };

    let reported = match result {
        Err(error @ RedbCacheBuildError::Storage { .. }) => Some(error.to_string()),
        Err(other) => {
            panic!("build() on a damaged db file ({damage}) must report Storage, got {other:?}")
        }
        Ok(_) => None,
    };

    // Nothing may be deleted or recreated behind the caller's back: recovering
    // or discarding the file is the caller's decision, and a cache that wipes a
    // file on open would destroy evidence (and any hope of salvage) silently.
    assert!(
        path.is_file(),
        "the damaged db file ({damage}) must be left on disk, not deleted by build()"
    );
    assert_eq!(
        file_len(path),
        len_before,
        "the damaged db file ({damage}) must be left untouched, not recreated by build()"
    );

    reported
}

/// As [`assert_no_panic`], for damage redb cannot open at all: the failure must
/// additionally be delivered as an `Err(Storage)` value.
fn assert_storage_error_not_panic(dir: &TempDir, name: &str, path: &Path, damage: &str) {
    assert!(
        assert_no_panic(dir, name, path, damage).is_some(),
        "build() unexpectedly SUCCEEDED on a damaged db file ({damage}); redb cannot \
         open this file, so either the fixture stopped damaging the file or redb gained \
         recovery (if the latter, relax this assertion to assert_no_panic)"
    );
}

// ── The bug: several kinds of damage panicked out of build() ─────────────────

#[test]
fn truncated_by_one_byte_is_an_error_not_a_panic() {
    let dir = scratch_dir();
    let path = make_healthy_db(&dir, "corrupt-truncate-1");
    truncate_by(&path, 1);
    assert_storage_error_not_panic(&dir, "corrupt-truncate-1", &path, "truncated by 1 byte");
}

#[test]
fn truncated_by_a_page_is_an_error_not_a_panic() {
    let dir = scratch_dir();
    let path = make_healthy_db(&dir, "corrupt-truncate-page");
    truncate_by(&path, 4096);
    assert_storage_error_not_panic(
        &dir,
        "corrupt-truncate-page",
        &path,
        "truncated by 4096 bytes",
    );
}

#[test]
fn flipped_header_byte_is_an_error_not_a_panic() {
    let dir = scratch_dir();
    let path = make_healthy_db(&dir, "corrupt-flip-header");
    flip_byte(&path, 16);
    assert_storage_error_not_panic(
        &dir,
        "corrupt-flip-header",
        &path,
        "flipped byte at offset 16",
    );
}

/// A flipped byte inside the data region. Whether redb rejects it depends on
/// what lives at that offset (with this fixture it happens to be readable), so
/// the requirement here is only the one that always holds: `build` returns a
/// value instead of unwinding, and leaves the file alone.
#[test]
fn flipped_page_byte_never_panics() {
    let dir = scratch_dir();
    let path = make_healthy_db(&dir, "corrupt-flip-page");
    flip_byte(&path, 4096);
    assert_no_panic(
        &dir,
        "corrupt-flip-page",
        &path,
        "flipped byte at offset 4096",
    );
}

/// Damage smeared across the first pages: flipping a byte in each of the first
/// pages hits redb's metadata regardless of the exact layout, so this stays
/// meaningful if a redb upgrade moves things around.
#[test]
fn flipped_bytes_across_early_pages_are_an_error_not_a_panic() {
    let dir = scratch_dir();
    let path = make_healthy_db(&dir, "corrupt-flip-spread");
    for offset in [16u64, 24, 4096, 8192, 12288] {
        flip_byte(&path, offset);
    }
    assert_storage_error_not_panic(
        &dir,
        "corrupt-flip-spread",
        &path,
        "flipped bytes at offsets 16/24/4096/8192/12288",
    );
}

// ── Baselines ────────────────────────────────────────────────────────────────

/// Damage redb already reported as an error (a file truncated to a stub) must
/// keep doing so — the fix must not convert a real error into something else.
#[test]
fn truncated_to_a_stub_is_still_an_error() {
    let dir = scratch_dir();
    let path = make_healthy_db(&dir, "corrupt-stub");
    let len = file_len(&path);
    truncate_by(&path, len - 64);
    assert_storage_error_not_panic(&dir, "corrupt-stub", &path, "truncated to 64 bytes");
}

/// The guard must not break the happy path: an undamaged file still opens, and
/// its contents are intact.
#[test]
fn healthy_file_still_reopens_and_reads() {
    let dir = scratch_dir();
    let path = make_healthy_db(&dir, "healthy-reopen");
    assert!(file_len(&path) > 0);

    let cache = build_catching_panic(&dir, "healthy-reopen")
        .expect("reopening a healthy file must not panic")
        .expect("reopening a healthy file must succeed");
    assert_eq!(
        cache.cache_get(&7).unwrap(),
        Some(70),
        "entries written before the reopen must still be readable"
    );
}
