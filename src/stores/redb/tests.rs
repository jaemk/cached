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

/// The stored entry is MessagePack in its compact form: a positional
/// 2-element array of `value` then `created_at`. The field names are not on
/// the wire, so the field ORDER is what a reader depends on. Reordering the
/// fields of `CachedDiskValue`, or switching a write site to
/// `rmp_serde::to_vec_named`, changes these bytes and makes every entry in an
/// existing 3.x cache file undecodable without a `DISK_FILE_VERSION` bump.
#[test]
fn cached_disk_value_is_a_positional_array_of_value_then_created_at() {
    let created_at = SystemTime::UNIX_EPOCH + Duration::from_secs(1);
    let stored = CachedDiskValue {
        value: "hi".to_string(),
        created_at,
    };
    let bytes = rmp_serde::to_vec(&stored).expect("serialize");
    // 0x92: 2-element fixarray (a named encoding would start 0x82, fixmap).
    // 0xa2 "hi": 2-byte fixstr. Then `created_at`, itself a positional
    // 2-element array of seconds-since-epoch and sub-second nanoseconds.
    assert_eq!(
        bytes,
        vec![0x92, 0xa2, b'h', b'i', 0x92, 0x01, 0x00],
        "the frozen 3.x entry is a positional array of `value` then `created_at`"
    );

    // The same bytes decode as a positional tuple in that order.
    let (value, (secs, nanos)): (String, (u64, u32)) =
        rmp_serde::from_slice(&bytes).expect("positional decode");
    assert_eq!(value, "hi");
    assert_eq!((secs, nanos), (1, 0));

    // The borrowed entry used by `cache_set_ref` must be byte-identical,
    // which requires it to keep the same fields in the same order.
    let borrowed = "hi".to_string();
    let borrowed_bytes = rmp_serde::to_vec(&CachedDiskValueRef {
        value: &borrowed,
        created_at,
    })
    .expect("serialize borrowed");
    assert_eq!(
        borrowed_bytes, bytes,
        "`cache_set_ref` must write the same positional layout as `cache_set`"
    );
}

#[test]
fn ttl_secs_and_ttl_millis_set_duration() {
    // No disk needed -- inspect the builder's ttl field without calling build().
    let b = RedbCache::<u32, u32>::builder("ttl-secs-builder").ttl_secs(7);
    assert_eq!(b.ttl, Some(Duration::from_secs(7)));

    let b = RedbCache::<u32, u32>::builder("ttl-millis-builder").ttl_millis(250);
    assert_eq!(b.ttl, Some(Duration::from_millis(250)));
}

#[test]
fn ttl_setters_override_last_writer_wins() {
    // ttl(secs=10) then ttl_secs(5) -> 5s
    let b = RedbCache::<u32, u32>::builder("ttl-override-a")
        .ttl(Duration::from_secs(10))
        .ttl_secs(5);
    assert_eq!(b.ttl, Some(Duration::from_secs(5)));

    // ttl_secs then ttl_millis -> the millis value
    let b = RedbCache::<u32, u32>::builder("ttl-override-b")
        .ttl_secs(10)
        .ttl_millis(500);
    assert_eq!(b.ttl, Some(Duration::from_millis(500)));

    // ttl_millis then ttl -> the ttl value
    let b = RedbCache::<u32, u32>::builder("ttl-override-c")
        .ttl_millis(500)
        .ttl(Duration::from_secs(3));
    assert_eq!(b.ttl, Some(Duration::from_secs(3)));
}

#[test]
fn new_returns_ready_cache_via_builder_with_ttl_secs() {
    // RedbCache has no `new()` (builder-only); the ttl_secs convenience
    // setter produces a working disk cache that respects the TTL.
    let dir = temp_dir!();
    let cache: RedbCache<u32, u32> = RedbCache::builder("ttl-secs-roundtrip")
        .disk_dir(dir.path())
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
    let cache: RedbCache<u32, u32> = RedbCache::builder("set-ttl-zero-disables")
        .disk_dir(dir.path())
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

#[test]
fn set_ttl_is_retroactive_for_existing_entries() {
    // redb persists only each entry's `created_at`, never an absolute expiry, so
    // liveness is recomputed at read time against the current ttl. Lowering the ttl
    // retroactively expires already-stored entries, and raising it revives an entry
    // the shorter ttl would have expired (as long as an expiring read has not already
    // evicted it). Neither entry is ever rewritten.
    let dir = temp_dir!();
    let cache: RedbCache<u32, u32> = RedbCache::builder("set-ttl-retroactive")
        .disk_dir(dir.path())
        .ttl_secs(3600)
        .build()
        .expect("build must succeed");
    assert_eq!(cache.cache_set(1, 100).unwrap(), None);
    assert_eq!(cache.cache_set(2, 200).unwrap(), None);
    assert_eq!(
        cache.cache_get(&1).unwrap(),
        Some(100),
        "entry must be live under the long ttl"
    );

    // Lower the ttl below both entries' current age: they are now expired even
    // though neither was rewritten.
    std::thread::sleep(Duration::from_millis(15));
    cache.set_ttl(Duration::from_millis(5));
    assert_eq!(
        cache.cache_get(&1).unwrap(),
        None,
        "lowering the ttl must retroactively expire the existing entry"
    );

    // Key 2 was not read while expired, so it is still on disk. Raise the ttl:
    // liveness is recomputed at read time from `created_at`, reviving it.
    cache.set_ttl(Duration::from_secs(3600));
    assert_eq!(
        cache.cache_get(&2).unwrap(),
        Some(200),
        "raising the ttl must revive an entry the shorter ttl would have expired"
    );
}

#[test]
fn unset_ttl_is_retroactive_and_revives_aged_entries() {
    // The `unset_ttl` doc claims disabling expiry is retroactive: it makes
    // every stored entry live regardless of age, including entries the prior
    // TTL would already have expired. This exercises the distinct
    // `unset_ttl()` method (not `set_ttl(ZERO)`), on an entry whose age has
    // clearly exceeded the TTL. Uses a never-read key so eviction-on-read
    // cannot pre-delete the entry and mask the revival.
    let dir = temp_dir!();
    let cache: RedbCache<u32, u32> = RedbCache::builder("unset-ttl-retroactive")
        .disk_dir(dir.path())
        .ttl_millis(5)
        .build()
        .expect("build must succeed");
    assert_eq!(cache.cache_set(1, 100).unwrap(), None);
    assert_eq!(cache.cache_set(2, 200).unwrap(), None);

    // Age both entries well past the 5ms TTL (sleep is a lower bound, so the
    // margin only grows; not time-fragile).
    std::thread::sleep(Duration::from_millis(40));

    // Key 1: read while the TTL is still active proves the entries are
    // genuinely expired (and evicts key 1).
    assert_eq!(
        cache.cache_get(&1).unwrap(),
        None,
        "entry older than the TTL must read as expired before unset"
    );

    // Key 2 was never read while expired, so it is still on disk. Disabling
    // the TTL must revive it: liveness is recomputed at read time and, with
    // no TTL, every stored entry is live.
    assert_eq!(cache.unset_ttl(), Some(Duration::from_millis(5)));
    assert_eq!(cache.ttl(), None);
    assert_eq!(
        cache.cache_get(&2).unwrap(),
        Some(200),
        "unset_ttl must retroactively revive an entry the prior TTL had expired"
    );
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
    let cache: RedbCache<u32, SerializeFailsAfterDeserialize> =
        RedbCache::builder("serialize_error_on_refresh")
            .disk_dir(tmp_dir.path())
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

/// D2: strict mode -- a corrupt entry returns `Err(CacheDeserialization)` and
/// leaves the entry in place.
#[test]
fn cache_get_returns_decode_error_for_corrupted_value_in_strict_mode() {
    let tmp_dir = temp_dir!();
    let cache: RedbCache<u32, u32> = RedbCache::builder("corrupted-cache-get-strict")
        .disk_dir(tmp_dir.path())
        .strict_deserialization(true)
        .build()
        .expect("error building disk cache");
    raw_insert(&cache, &TEST_KEY.to_string(), vec![0xc1, 0xc1, 0xc1]);

    assert!(matches!(
        cache.cache_get(&TEST_KEY),
        Err(RedbCacheError::CacheDeserialization { .. })
    ));
    // In strict mode, the corrupt entry must NOT be deleted.
    assert!(raw_get(&cache, &TEST_KEY.to_string()).is_some());
}

/// D2: default mode -- a corrupt entry is self-healed: deleted and Ok(None) returned.
#[test]
fn cache_get_self_heals_corrupted_entry_by_default() {
    let tmp_dir = temp_dir!();
    let cache: RedbCache<u32, u32> = RedbCache::builder("corrupted-cache-get-default")
        .disk_dir(tmp_dir.path())
        .build()
        .expect("error building disk cache");
    raw_insert(&cache, &TEST_KEY.to_string(), vec![0xc1, 0xc1, 0xc1]);

    // Default mode: self-heal returns Ok(None).
    assert!(matches!(cache.cache_get(&TEST_KEY), Ok(None)));
    // The corrupt entry must have been deleted.
    assert!(raw_get(&cache, &TEST_KEY.to_string()).is_none());
}

#[test]
fn cache_delete_removes_corrupted_value_without_decoding() {
    let tmp_dir = temp_dir!();
    let cache: RedbCache<u32, u32> = RedbCache::builder("corrupted-cache-delete")
        .disk_dir(tmp_dir.path())
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
    let cache: RedbCache<u32, u32> = RedbCache::builder("corrupted-cache-set")
        .disk_dir(tmp_dir.path())
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
    let cache: RedbCache<u32, u32> = RedbCache::builder("corrupted-cache-remove")
        .disk_dir(tmp_dir.path())
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
    let cache: RedbCache<u32, u32> = RedbCache::builder("remove-entry-roundtrip")
        .disk_dir(tmp_dir.path())
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
    let cache: RedbCache<u32, u32> = RedbCache::builder("remove-entry-expired")
        .disk_dir(tmp_dir.path())
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
    let cache: RedbCache<u32, u32> = RedbCache::builder("flush-test")
        .disk_dir(tmp_dir.path())
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
    let reopened: RedbCache<u32, u32> = RedbCache::builder("flush-test")
        .disk_dir(tmp_dir.path())
        .build()
        .expect("error re-opening cache");
    assert_that!(reopened.cache_get(&TEST_KEY), ok(some(eq(&TEST_VAL))));

    // flush is a safe no-op on an already-durable cache, and on an empty cache.
    let durable: RedbCache<u32, u32> = RedbCache::builder("flush-test-durable")
        .disk_dir(tmp_dir.path())
        .durable(true)
        .build()
        .unwrap();
    durable
        .flush()
        .expect("flush on a durable/empty cache should succeed");
}

// SEC-4: a symlink planted at the db path must not be followed. The symlink
// targets a VALID redb file so that, absent the check, `build` would happily
// open it through the link; only the symlink rejection makes this fail.
#[cfg(unix)]
#[test]
fn symlinked_db_file_is_rejected() {
    const NAME: &str = "symlink-dbfile";
    let file_name = format!("{NAME}_v{DISK_FILE_VERSION}.redb");

    // Build a real, valid redb file in a separate directory, then drop it to
    // release redb's exclusive lock.
    let target = temp_dir!();
    {
        let _seed: RedbCache<u32, u32> = RedbCache::builder(NAME)
            .disk_dir(target.path())
            .build()
            .unwrap();
    }
    let target_file = target.path().join(&file_name);

    let dir = temp_dir!();
    std::os::unix::fs::symlink(&target_file, dir.path().join(&file_name)).unwrap();

    let result = RedbCache::<u32, u32>::builder(NAME)
        .disk_dir(dir.path())
        .build();
    assert!(result.is_err(), "a symlinked db path must be rejected");
}

// SEC-4: an explicitly configured directory that is a symlink is rejected.
#[cfg(unix)]
#[test]
fn symlinked_disk_dir_is_rejected() {
    let real = temp_dir!();
    let link_parent = temp_dir!();
    let link = link_parent.path().join("linkdir");
    std::os::unix::fs::symlink(real.path(), &link).unwrap();

    let result = RedbCache::<u32, u32>::builder("symlink-dir")
        .disk_dir(&link)
        .build();
    assert!(
        result.is_err(),
        "a symlinked cache directory must be rejected"
    );
}

// DISK-7: a pre-existing db file (e.g. an rc-era 0644 file) is chmodded to
// 0600 on open, not left readable by group/other.
#[cfg(unix)]
#[test]
fn preexisting_db_file_is_chmodded_to_0600() {
    use std::os::unix::fs::PermissionsExt;
    const NAME: &str = "chmod-existing";
    let file_name = format!("{NAME}_v{DISK_FILE_VERSION}.redb");
    let dir = temp_dir!();
    let path = dir.path().join(&file_name);
    // Simulate an rc-era file created world/group-readable.
    std::fs::write(&path, b"").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

    let _c: RedbCache<u32, u32> = RedbCache::builder(NAME)
        .disk_dir(dir.path())
        .build()
        .unwrap();

    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "pre-existing db file must be chmodded to 0600");
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
    let a: RedbCache<u32, u32> = RedbCache::builder(NAME)
        .disk_dir(dir_a.path())
        .durable(false) // opt into Durability::None writes
        .build()
        .unwrap();
    a.cache_set(TEST_KEY, TEST_VAL).unwrap(); // Durability::None (not yet durable)

    // Snapshot before flush: a fresh instance on the copy must NOT see the entry.
    let dir_before = temp_dir!();
    std::fs::copy(&src, dir_before.path().join(&file_name)).unwrap();
    let before: RedbCache<u32, u32> = RedbCache::builder(NAME)
        .disk_dir(dir_before.path())
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
    let after: RedbCache<u32, u32> = RedbCache::builder(NAME)
        .disk_dir(dir_after.path())
        .build()
        .unwrap();
    assert_that!(
        after.cache_get(&TEST_KEY),
        ok(some(eq(&TEST_VAL))),
        "after flush the write is durable and visible to a fresh instance"
    );
}

/// D2: strict mode -- `remove_expired_entries` aborts on the first corrupt entry.
#[test]
fn remove_expired_entries_returns_decode_error_for_corrupted_value_in_strict_mode() {
    let tmp_dir = temp_dir!();
    let cache: RedbCache<u32, u32> = RedbCache::builder("corrupted-sweep-strict")
        .disk_dir(tmp_dir.path())
        .ttl(Duration::from_secs(1))
        .strict_deserialization(true)
        .build()
        .expect("error building disk cache");
    raw_insert(&cache, &TEST_KEY.to_string(), vec![0xc1, 0xc1, 0xc1]);

    assert!(matches!(
        cache.remove_expired_entries(),
        Err(RedbCacheError::CacheDeserialization { .. })
    ));
}

/// D2: default mode -- `remove_expired_entries` treats corrupt entries as
/// eviction candidates (removes them and counts them).
#[test]
fn remove_expired_entries_self_heals_corrupted_entries_by_default() {
    let tmp_dir = temp_dir!();
    let cache: RedbCache<u32, u32> = RedbCache::builder("corrupted-sweep-default")
        .disk_dir(tmp_dir.path())
        .ttl(Duration::from_secs(1))
        .build()
        .expect("error building disk cache");
    raw_insert(&cache, &TEST_KEY.to_string(), vec![0xc1, 0xc1, 0xc1]);

    // Default mode: corrupt entry is treated as eviction candidate.
    let count = cache
        .remove_expired_entries()
        .expect("remove_expired_entries must succeed");
    assert_eq!(count, 1, "corrupt entry must be counted as removed");
    // The corrupt entry must be gone.
    assert!(raw_get(&cache, &TEST_KEY.to_string()).is_none());
}

#[test]
fn remove_expired_entries_returns_count_of_removed_entries() {
    let tmp_dir = temp_dir!();
    let cache: RedbCache<u32, u32> = RedbCache::builder("sweep-count")
        .disk_dir(tmp_dir.path())
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

/// B3 regression: `remove_expired_entries` must not remove an entry that was
/// refreshed between the scan pass and the write pass.  Both passes must use the
/// same `now` snapshot so an entry that looks expired in the scan but is
/// refreshed before the write does not get deleted.
///
/// This test cannot directly observe two `SystemTime::now()` calls, but it
/// verifies the externally observable invariant: a freshly written entry that
/// crosses the TTL boundary must NOT be deleted by a concurrent sweep that
/// started before the refresh.  (The single-snapshot fix makes the scan and
/// write agree on the same instant, so a refresh that happens between the two
/// passes is safe provided the refreshed timestamp is after that snapshot.)
#[test]
fn remove_expired_entries_uses_single_time_snapshot() {
    let tmp_dir = temp_dir!();
    let cache: RedbCache<u32, u32> = RedbCache::builder("sweep-single-snapshot")
        .disk_dir(tmp_dir.path())
        .ttl(LIFE_SPAN_1_SEC)
        .build()
        .expect("error building disk cache");

    cache.cache_set(1, 10).unwrap();
    cache.cache_set(2, 20).unwrap();

    sleep(LIFE_SPAN_1_SEC + Duration::from_millis(50));

    // Refresh key 1 just before the sweep — it should survive.
    cache.cache_set(1, 11).unwrap();

    // The sweep was started conceptually before the refresh, but because a
    // single snapshot is taken at sweep entry time, key 1 was refreshed
    // before `now` in the write pass and must NOT be removed.
    // Key 2 remains expired and must be removed.
    let removed = cache.remove_expired_entries().unwrap();
    assert_eq!(removed, 1, "only the expired entry must be removed");
    assert!(
        raw_get(&cache, &1u32.to_string()).is_some(),
        "refreshed entry must survive the sweep"
    );
    assert!(
        raw_get(&cache, &2u32.to_string()).is_none(),
        "stale entry must be removed by the sweep"
    );
}

/// DISK-5: `remove_expired_entries` returns `Ok(0)` immediately when no TTL
/// is configured, without scanning any entries.
#[test]
fn remove_expired_entries_returns_zero_when_no_ttl_configured() {
    let tmp_dir = temp_dir!();
    let cache: RedbCache<u32, u32> = RedbCache::builder("sweep-no-ttl")
        .disk_dir(tmp_dir.path())
        // No TTL set -- entries never expire.
        .build()
        .expect("error building disk cache");
    cache.cache_set(1, 10).unwrap();
    cache.cache_set(2, 20).unwrap();

    let removed = cache.remove_expired_entries().expect("sweep must succeed");
    assert_eq!(removed, 0, "no TTL means no entries expire");
    // Entries are still present.
    assert!(raw_get(&cache, &1u32.to_string()).is_some());
    assert!(raw_get(&cache, &2u32.to_string()).is_some());
}

/// D2 + sweep: one `remove_expired_entries` call over a table holding a valid
/// *live* entry, a valid *expired* entry, and two *corrupt* entries must remove
/// exactly the expired and both corrupt ones (count == 3), retain the live
/// entry, and physically delete the rest. This is the interleaved-eviction
/// case the single-fixture tests above do not exercise.
#[test]
fn remove_expired_entries_sweeps_corrupt_and_expired_but_keeps_live() {
    let tmp_dir = temp_dir!();
    let cache: RedbCache<u32, u32> = RedbCache::builder("sweep-mixed-corrupt-live-expired")
        .disk_dir(tmp_dir.path())
        .ttl(Duration::from_secs(60))
        .build()
        .expect("error building disk cache");

    // Live entry: freshly written, age ~0 << 60s ttl -> must be retained.
    cache.cache_set(1, 10).unwrap();

    // Valid-but-expired entry: a well-formed CachedDiskValue whose created_at
    // is an hour in the past (age >> ttl) -> must be removed. Constructed by
    // hand so the age is deterministic rather than sleep-dependent.
    let expired = CachedDiskValue {
        value: 20u32,
        created_at: SystemTime::now()
            .checked_sub(Duration::from_secs(3600))
            .expect("subtracting an hour must not underflow"),
    };
    raw_insert(
        &cache,
        &2u32.to_string(),
        rmp_serde::to_vec(&expired).expect("serialize expired fixture"),
    );

    // Two corrupt entries (distinct undecodable byte patterns) -> both removed
    // in default (self-heal) mode.
    raw_insert(&cache, &3u32.to_string(), vec![0xc1, 0xc1, 0xc1]);
    raw_insert(&cache, &4u32.to_string(), vec![0xc1, 0x00, 0xff]);

    // Exactly three entries removed: the expired one and the two corrupt ones.
    assert_eq!(
        cache.remove_expired_entries().unwrap(),
        3,
        "sweep must remove the expired entry and both corrupt entries, and only those"
    );

    // The live entry survives and stays readable; every other key is gone.
    assert!(
        raw_get(&cache, &1u32.to_string()).is_some(),
        "live entry must be retained"
    );
    assert_eq!(
        cache.cache_get(&1).unwrap(),
        Some(10),
        "the retained live entry must still decode"
    );
    assert!(
        raw_get(&cache, &2u32.to_string()).is_none(),
        "expired entry must be physically removed"
    );
    assert!(
        raw_get(&cache, &3u32.to_string()).is_none(),
        "first corrupt entry must be physically removed"
    );
    assert!(
        raw_get(&cache, &4u32.to_string()).is_none(),
        "second corrupt entry must be physically removed"
    );
}

/// D2: strict-mode `remove_expired_entries` aborts on the first corrupt entry
/// AND leaves it physically in place — it must not evict in strict mode.
/// The existing strict sweep test only asserts the error; this pins the
/// no-delete side of the contract.
#[test]
fn remove_expired_entries_strict_mode_aborts_and_leaves_corrupt_entry() {
    let tmp_dir = temp_dir!();
    let cache: RedbCache<u32, u32> = RedbCache::builder("sweep-strict-leaves-corrupt")
        .disk_dir(tmp_dir.path())
        .ttl(Duration::from_secs(1))
        .strict_deserialization(true)
        .build()
        .expect("error building disk cache");
    raw_insert(&cache, &TEST_KEY.to_string(), vec![0xc1, 0xc1, 0xc1]);

    assert!(matches!(
        cache.remove_expired_entries(),
        Err(RedbCacheError::CacheDeserialization { .. })
    ));
    assert!(
        raw_get(&cache, &TEST_KEY.to_string()).is_some(),
        "strict-mode sweep must leave the corrupt entry in place"
    );
}

/// A strict-mode sweep that errors on a corrupt entry must leave the store
/// exactly as it was: the whole sweep runs in one redb write transaction, so
/// validly-expired entries removed earlier in the pass are rolled back when
/// the transaction aborts.
#[test]
fn remove_expired_entries_strict_mode_error_rolls_back_prior_evictions() {
    let tmp_dir = temp_dir!();
    let cache: RedbCache<u32, u32> = RedbCache::builder("sweep-strict-rolls-back")
        .disk_dir(tmp_dir.path())
        .ttl(LIFE_SPAN_1_SEC)
        .strict_deserialization(true)
        .build()
        .expect("error building disk cache");

    // A valid entry that will be expired by sweep time. Its key ("1") sorts
    // before the corrupt entry's key ("5"), so the sweep visits it first.
    cache.cache_set(1, 10).unwrap();
    sleep(LIFE_SPAN_1_SEC + Duration::from_millis(50));
    raw_insert(&cache, &5u32.to_string(), vec![0xc1, 0xc1, 0xc1]);

    assert!(matches!(
        cache.remove_expired_entries(),
        Err(RedbCacheError::CacheDeserialization { .. })
    ));
    assert!(
        raw_get(&cache, &1u32.to_string()).is_some(),
        "the expired-but-valid entry must be rolled back, not removed"
    );
    assert!(
        raw_get(&cache, &5u32.to_string()).is_some(),
        "the corrupt entry must be left in place"
    );

    // A follow-up sweep in self-heal terms: removing the corrupt entry by key
    // unblocks the sweep, which then removes the expired entry.
    assert!(cache.cache_delete(&5).unwrap());
    assert_eq!(cache.remove_expired_entries().unwrap(), 1);
    assert!(raw_get(&cache, &1u32.to_string()).is_none());
}

/// D2: after a self-healed miss the corrupt entry is gone; recomputing via
/// `cache_set` and reading again must produce a HIT. Guards that the self-heal
/// delete does not poison subsequent writes to the same key.
#[test]
fn cache_get_self_heal_then_recompute_produces_hit() {
    let tmp_dir = temp_dir!();
    let cache: RedbCache<u32, u32> = RedbCache::builder("self-heal-then-recompute")
        .disk_dir(tmp_dir.path())
        .build()
        .expect("error building disk cache");
    raw_insert(&cache, &TEST_KEY.to_string(), vec![0xc1, 0xc1, 0xc1]);

    // Self-heal: corrupt entry -> Ok(None) and the entry is deleted.
    assert_eq!(cache.cache_get(&TEST_KEY).unwrap(), None);
    assert!(raw_get(&cache, &TEST_KEY.to_string()).is_none());

    // Recompute over the healed miss: the previous value is None.
    assert_eq!(cache.cache_set(TEST_KEY, TEST_VAL).unwrap(), None);
    // Subsequent read is a HIT with the recomputed value.
    assert_eq!(cache.cache_get(&TEST_KEY).unwrap(), Some(TEST_VAL));
}

/// `RedbCacheError::is_deserialization` classifies only the decode variant,
/// not storage or serialization errors. No disk needed.
#[test]
fn is_deserialization_classifier_distinguishes_variants() {
    let bad: Vec<u8> = vec![0xc1];
    let deser =
        RedbCacheError::deserialization(rmp_serde::from_slice::<u32>(&bad).unwrap_err(), bad);
    assert!(
        deser.is_deserialization(),
        "decode error must classify true"
    );

    let storage = RedbCacheError::storage(redb::Error::Io(std::io::Error::other("io")));
    assert!(
        !storage.is_deserialization(),
        "storage error must classify false"
    );

    let ser = RedbCacheError::serialization(std::io::Error::other("ser"));
    assert!(
        !ser.is_deserialization(),
        "serialization error must classify false"
    );
}

const LIFE_SPAN_2_SECS: Duration = Duration::from_secs(2);
const LIFE_SPAN_1_SEC: Duration = Duration::from_secs(1);
#[googletest::test]
fn cache_get_after_cache_remove_returns_none() {
    let tmp_dir = temp_dir!();
    let cache: RedbCache<u32, u32> = RedbCache::builder("test-cache")
        .disk_dir(tmp_dir.path())
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
    let cache: RedbCache<u32, u32> = RedbCache::builder("test-cache-clear")
        .disk_dir(tmp_dir.path())
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
    let cache: RedbCache<u32, u32> = RedbCache::builder("test-cache")
        .disk_dir(tmp_dir.path())
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
    let cache: RedbCache<u32, u32> = RedbCache::builder("test-cache")
        .disk_dir(tmp_dir.path())
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
    let cache: RedbCache<u32, u32> = RedbCache::builder("test-cache")
        .disk_dir(tmp_dir.path())
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
// round-trip succeeds when `disk_dir` is left at its default.
fn does_not_break_when_constructed_using_default_disk_dir() {
    let cache: RedbCache<u32, u32> =
        RedbCache::builder(format!("{}-disk-cache-test-default-dir", now_millis()))
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
                let cache: RedbCache<u32, u32> = RedbCache::builder(CACHE_NAME)
                    .disk_dir(cache_tmp_dir.path())
                    .durable(set_durable) // WHAT'S BEING TESTED
                    .build()
                    .unwrap();

                run_on_original_cache(&cache);
                // Drop the original cache so its exclusive lock on the redb
                // file is released before we re-open it below.
            }

            let recovered_cache: RedbCache<u32, u32> = RedbCache::builder(CACHE_NAME)
                .disk_dir(cache_tmp_dir.path())
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
    let cache: RedbCache<u32, u32> = RedbCache::builder("test-cache-async")
        .disk_dir(tmp_dir.path())
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
    let cache: RedbCache<u32, u32> = RedbCache::builder("remove-entry-async")
        .disk_dir(tmp_dir.path())
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
    let cache: RedbCache<u32, u32> = RedbCache::builder("set-ref-roundtrip")
        .disk_dir(tmp_dir.path())
        .build()
        .expect("error building disk cache");

    let key = TEST_KEY;
    let value = TEST_VAL;
    // cache_set_ref writes from a borrow and returns `()` (no previous value).
    assert_that!(
        crate::SerializeCached::cache_set_ref(&cache, &key, &value),
        ok(eq(&())),
        "cache_set_ref on a new key should return Ok(())"
    );
    // cache_get must return the value that was written via cache_set_ref.
    assert_that!(
        cache.cache_get(&key),
        ok(some(eq(&value))),
        "cache_get after cache_set_ref should return the written value"
    );
    // A second cache_set_ref displaces the first; it still returns `()`.
    let value2 = TEST_VAL_1;
    assert_that!(
        crate::SerializeCached::cache_set_ref(&cache, &key, &value2),
        ok(eq(&())),
        "cache_set_ref over an existing entry should return Ok(())"
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
    let cache: RedbCache<u32, u32> = RedbCache::builder("debug-smoke")
        .disk_dir(tmp_dir.path())
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
            RedbCache::<u32, u32>::builder("")
                .disk_dir(tmp_dir.path())
                .build(),
            Err(RedbCacheBuildError::InvalidCacheName)
        ),
        "empty cache_name must return InvalidCacheName"
    );

    assert!(
        matches!(
            RedbCache::<u32, u32>::builder("bad/name")
                .disk_dir(tmp_dir.path())
                .build(),
            Err(RedbCacheBuildError::InvalidCacheName)
        ),
        "cache_name containing '/' must return InvalidCacheName"
    );

    // ':' is now rejected: it is invalid in NTFS/Windows filenames, so a
    // module-path-derived name such as "a::b" no longer builds.
    assert!(
        matches!(
            RedbCache::<u32, u32>::builder("ok:name")
                .disk_dir(tmp_dir.path())
                .build(),
            Err(RedbCacheBuildError::InvalidCacheName)
        ),
        "cache_name containing ':' must return InvalidCacheName"
    );

    // Other NTFS/Windows-reserved characters are rejected too.
    assert!(
        matches!(
            RedbCache::<u32, u32>::builder("star*name")
                .disk_dir(tmp_dir.path())
                .build(),
            Err(RedbCacheBuildError::InvalidCacheName)
        ),
        "cache_name containing '*' must return InvalidCacheName"
    );

    assert!(
        matches!(
            RedbCache::<u32, u32>::builder("q?name")
                .disk_dir(tmp_dir.path())
                .build(),
            Err(RedbCacheBuildError::InvalidCacheName)
        ),
        "cache_name containing '?' must return InvalidCacheName"
    );

    assert!(
        matches!(
            RedbCache::<u32, u32>::builder("lt<name")
                .disk_dir(tmp_dir.path())
                .build(),
            Err(RedbCacheBuildError::InvalidCacheName)
        ),
        "cache_name containing '<' must return InvalidCacheName"
    );

    assert!(
        matches!(
            RedbCache::<u32, u32>::builder("bad\\name")
                .disk_dir(tmp_dir.path())
                .build(),
            Err(RedbCacheBuildError::InvalidCacheName)
        ),
        "cache_name containing '\\\\' must return InvalidCacheName"
    );

    assert!(
        matches!(
            RedbCache::<u32, u32>::builder("..")
                .disk_dir(tmp_dir.path())
                .build(),
            Err(RedbCacheBuildError::InvalidCacheName)
        ),
        "cache_name '..' must return InvalidCacheName"
    );

    assert!(
        matches!(
            RedbCache::<u32, u32>::builder(".")
                .disk_dir(tmp_dir.path())
                .build(),
            Err(RedbCacheBuildError::InvalidCacheName)
        ),
        "cache_name '.' must return InvalidCacheName"
    );

    // A valid name must still build successfully.
    assert!(
        RedbCache::<u32, u32>::builder("valid-cache-name")
            .disk_dir(tmp_dir.path())
            .build()
            .is_ok(),
        "a valid cache_name must build successfully"
    );

    // Still-allowed characters (alphanumerics, '_', '-') must build.
    assert!(
        RedbCache::<u32, u32>::builder("My_Cache-Name_123")
            .disk_dir(tmp_dir.path())
            .build()
            .is_ok(),
        "cache_name of alphanumerics, '_' and '-' must build successfully"
    );
}

#[test]
fn build_rejects_cache_name_with_nul_byte() {
    let tmp_dir = temp_dir!();

    assert!(
        matches!(
            RedbCache::<u32, u32>::builder("bad\0name")
                .disk_dir(tmp_dir.path())
                .build(),
            Err(RedbCacheBuildError::InvalidCacheName)
        ),
        "cache_name containing a NUL byte must return InvalidCacheName"
    );
}

// Certification helper: assert a given cache_name is rejected with
// InvalidCacheName. Kept as a committed helper (not an inline probe) so the
// per-character rejection checks below stay uniform.
fn assert_name_rejected(name: &str, why: &str) {
    let tmp_dir = temp_dir!();
    assert!(
        matches!(
            RedbCache::<u32, u32>::builder(name)
                .disk_dir(tmp_dir.path())
                .build(),
            Err(RedbCacheBuildError::InvalidCacheName)
        ),
        "{why}"
    );
}

// Certification helper: assert a given cache_name is accepted (build succeeds).
fn assert_name_accepted(name: &str, why: &str) {
    let tmp_dir = temp_dir!();
    assert!(
        RedbCache::<u32, u32>::builder(name)
            .disk_dir(tmp_dir.path())
            .build()
            .is_ok(),
        "{why}"
    );
}

#[test]
fn build_rejects_remaining_reserved_filename_chars() {
    // The existing test covers '/', '\\', ':', '*', '?', '<'. These three
    // reserved characters ('>', '"', '|') are the rest of the newly-rejected
    // NTFS/Windows set and were previously untested.
    assert_name_rejected("gt>name", "cache_name containing '>' must be rejected");
    assert_name_rejected("quote\"name", "cache_name containing '\"' must be rejected");
    assert_name_rejected("pipe|name", "cache_name containing '|' must be rejected");
}

#[test]
fn build_rejects_non_nul_control_chars() {
    // Only NUL (0x00) was previously tested. Cover the rest of the ASCII
    // control range that `is_ascii_control()` rejects: a mid-range control
    // (TAB, 0x09), the top of the C0 range (Unit Separator, 0x1F), and DEL
    // (0x7F).
    assert_name_rejected(
        "tab\tname",
        "cache_name containing TAB (0x09) must be rejected",
    );
    assert_name_rejected(
        "us\u{1f}name",
        "cache_name containing 0x1F must be rejected",
    );
    // `char::is_ascii_control()` rejects DEL (U+007F) as well as the C0 range,
    // which the doc calls out explicitly.
    assert_name_rejected(
        "del\u{7f}name",
        "cache_name containing DEL (0x7F) is rejected by is_ascii_control()",
    );
}

#[test]
fn build_does_not_over_reject_valid_names() {
    // Guard against a naive tightening that would reject any '.' or other
    // still-legal characters. Only the *bare* components "." and ".." are
    // rejected; a dot INSIDE a name is fine.
    assert_name_accepted(
        "a.b",
        "a '.' inside a name (not a bare '.'/'..') must build successfully",
    );
    assert_name_accepted("v1.2.3", "multiple interior dots must build successfully");
    // '~' is not reserved and not a control char, so it is accepted.
    assert_name_accepted("home~cache", "'~' must build successfully");
    // A space is neither reserved nor a control char, so it is currently
    // accepted. Locking this documents that spaces are not over-rejected.
    assert_name_accepted(
        "my cache",
        "a space in the name is currently accepted and must not be over-rejected",
    );
}

#[cfg(feature = "async")]
#[tokio::test]
async fn async_cache_set_ref_round_trips() {
    use crate::SerializeCachedAsync;

    let tmp_dir = temp_dir!();
    let cache: RedbCache<u32, u32> = RedbCache::builder("set-ref-roundtrip-async")
        .disk_dir(tmp_dir.path())
        .build()
        .expect("error building disk cache");

    let key = TEST_KEY;
    let value = TEST_VAL;
    // async_cache_set_ref writes from a borrow and returns `()` (no previous value).
    assert_eq!(
        cache.async_cache_set_ref(&key, &value).await.unwrap(),
        (),
        "async_cache_set_ref on a new key should return Ok(())"
    );
    // async_cache_get must return the value that was written via async_cache_set_ref.
    use crate::ConcurrentCachedAsync;
    assert_eq!(
        cache.async_cache_get(&key).await.unwrap(),
        Some(value),
        "async_cache_get after async_cache_set_ref should return the written value"
    );
    // A second async_cache_set_ref displaces the first; it still returns `()`.
    let value2 = TEST_VAL_1;
    assert_eq!(
        cache.async_cache_set_ref(&key, &value2).await.unwrap(),
        (),
        "async_cache_set_ref over an existing entry should return Ok(())"
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
    let cache: RedbCache<u32, u32> = RedbCache::builder("flush-test-async")
        .disk_dir(tmp_dir.path())
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

/// Plant an entry with a fixed `created_at` so `scan_stats` results do not
/// depend on timing.
fn raw_insert_at(cache: &RedbCache<u32, u32>, key: &str, value: u32, created_at: SystemTime) {
    let stored = CachedDiskValue { value, created_at };
    raw_insert(
        cache,
        key,
        rmp_serde::to_vec(&stored).expect("error serializing fixture"),
    );
}

#[test]
fn scan_stats_on_empty_cache_is_all_zero() {
    let tmp_dir = temp_dir!();
    let cache: RedbCache<u32, u32> = RedbCache::builder("scan-stats-empty")
        .disk_dir(tmp_dir.path())
        .ttl(Duration::from_secs(60))
        .build()
        .unwrap();

    assert_eq!(cache.scan_stats().unwrap(), RedbCacheStats::default());
}

#[test]
fn scan_stats_reports_count_and_oldest_newest() {
    let tmp_dir = temp_dir!();
    let cache: RedbCache<u32, u32> = RedbCache::builder("scan-stats-count")
        .disk_dir(tmp_dir.path())
        .build()
        .unwrap();
    let t1 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
    let t2 = SystemTime::UNIX_EPOCH + Duration::from_secs(2_000);
    let t3 = SystemTime::UNIX_EPOCH + Duration::from_secs(3_000);
    // Inserted out of order so the result cannot come from key order.
    raw_insert_at(&cache, "a", 1, t2);
    raw_insert_at(&cache, "b", 2, t3);
    raw_insert_at(&cache, "c", 3, t1);

    let stats = cache.scan_stats().unwrap();
    assert_eq!(stats.entries, 3);
    assert_eq!(stats.expired, 0, "no ttl, so nothing is expired");
    assert_eq!(stats.undecodable, 0);
    assert_eq!(stats.oldest, Some(t1));
    assert_eq!(stats.newest, Some(t3));
}

#[test]
fn scan_stats_counts_expired_against_current_ttl() {
    let tmp_dir = temp_dir!();
    let cache: RedbCache<u32, u32> = RedbCache::builder("scan-stats-expired")
        .disk_dir(tmp_dir.path())
        .ttl(Duration::from_secs(60))
        .build()
        .unwrap();
    let now = SystemTime::now();
    let old = now - Duration::from_secs(120);
    raw_insert_at(&cache, "old", 1, old);
    raw_insert_at(&cache, "fresh", 2, now);

    let stats = cache.scan_stats().unwrap();
    assert_eq!(stats.entries, 2);
    assert_eq!(stats.expired, 1);
    // Expired entries still count toward oldest/newest.
    assert_eq!(stats.oldest, Some(old));
    assert_eq!(stats.newest, Some(now));

    // TTL is retroactive: raising it revives the old entry.
    cache.set_ttl(Duration::from_secs(300));
    assert_eq!(cache.scan_stats().unwrap().expired, 0);

    cache.set_ttl(Duration::from_secs(60));
    cache.unset_ttl();
    assert_eq!(cache.scan_stats().unwrap().expired, 0);
}

#[test]
fn scan_stats_counts_undecodable_without_removing_in_both_modes() {
    for strict in [false, true] {
        let tmp_dir = temp_dir!();
        let cache: RedbCache<u32, u32> = RedbCache::builder("scan-stats-undecodable")
            .disk_dir(tmp_dir.path())
            .ttl(Duration::from_secs(60))
            .strict_deserialization(strict)
            .build()
            .unwrap();
        let t = SystemTime::now();
        raw_insert_at(&cache, "ok", 1, t);
        // Not MessagePack at all.
        raw_insert(&cache, "garbage", vec![0xc1, 0xc1, 0xc1]);
        // Valid MessagePack with a valid `created_at`, but the value is not a `u32`:
        // `cache_get` would reject it, so the scan must too.
        let wrong_type = CachedDiskValue {
            value: "not a u32".to_string(),
            created_at: SystemTime::UNIX_EPOCH,
        };
        raw_insert(
            &cache,
            "wrong-type",
            rmp_serde::to_vec(&wrong_type).unwrap(),
        );

        let stats = cache
            .scan_stats()
            .unwrap_or_else(|e| panic!("strict={strict}: scan must not error: {e}"));
        assert_eq!(stats.entries, 3, "strict={strict}");
        assert_eq!(stats.undecodable, 2, "strict={strict}");
        assert_eq!(
            stats.expired, 0,
            "strict={strict}: undecodable is not expired"
        );
        assert_eq!(stats.oldest, Some(t), "strict={strict}");
        assert_eq!(stats.newest, Some(t), "strict={strict}");
        assert!(raw_get(&cache, "garbage").is_some(), "strict={strict}");
        assert!(raw_get(&cache, "wrong-type").is_some(), "strict={strict}");
    }
}

/// The scan must not refresh, evict, or heal anything, even with
/// `refresh_on_hit` on and expired/corrupt entries present.
#[test]
fn scan_stats_does_not_write() {
    let tmp_dir = temp_dir!();
    let cache: RedbCache<u32, u32> = RedbCache::builder("scan-stats-read-only")
        .disk_dir(tmp_dir.path())
        .ttl(Duration::from_secs(60))
        .refresh_on_hit(true)
        .build()
        .unwrap();
    let now = SystemTime::now();
    raw_insert_at(&cache, "fresh", 1, now);
    raw_insert_at(&cache, "old", 2, now - Duration::from_secs(120));
    raw_insert(&cache, "garbage", vec![0xc1, 0xc1, 0xc1]);
    let before: Vec<_> = ["fresh", "old", "garbage"]
        .iter()
        .map(|k| raw_get(&cache, k))
        .collect();

    cache.scan_stats().unwrap();

    let after: Vec<_> = ["fresh", "old", "garbage"]
        .iter()
        .map(|k| raw_get(&cache, k))
        .collect();
    assert_eq!(before, after);
}

#[cfg(feature = "async")]
#[tokio::test]
async fn async_scan_stats_matches_scan_stats() {
    let tmp_dir = temp_dir!();
    let cache: RedbCache<u32, u32> = RedbCache::builder("scan-stats-async")
        .disk_dir(tmp_dir.path())
        .ttl(Duration::from_secs(60))
        .build()
        .unwrap();
    let now = SystemTime::now();
    raw_insert_at(&cache, "fresh", 1, now);
    raw_insert_at(&cache, "old", 2, now - Duration::from_secs(120));
    raw_insert(&cache, "garbage", vec![0xc1, 0xc1, 0xc1]);

    let stats = cache.async_scan_stats().await.unwrap();
    assert_eq!(stats.entries, 3);
    assert_eq!(stats.expired, 1);
    assert_eq!(stats.undecodable, 1);
    assert_eq!(stats, cache.scan_stats().unwrap());
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
    let cache: RedbCache<u32, u32> = RedbCache::builder("futures-block-on-test")
        .disk_dir(tmp_dir.path())
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
    // Construct the variant via constructor to verify it compiles.
    let err = RedbCacheBuildError::storage(redb::Error::Io(std::io::Error::other(
        "synthetic redb io error",
    )));
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
    // strict_deserialization=true so we get an error to inspect.
    let cache: RedbCache<u32, u32> = RedbCache::builder("decode-error-carries-bytes")
        .disk_dir(tmp_dir.path())
        .strict_deserialization(true)
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
    // In strict mode, the entry must still be present.
    assert!(raw_get(&cache, &TEST_KEY.to_string()).is_some());
}

/// `RedbCacheError::CacheDeserialization` from `remove_expired_entries`
/// also carries the raw bytes via the `cached_value` field.
#[test]
fn remove_expired_entries_decode_error_carries_raw_bytes() {
    let tmp_dir = temp_dir!();
    // strict_deserialization=true so we get an error to inspect.
    let cache: RedbCache<u32, u32> = RedbCache::builder("sweep-decode-error-bytes")
        .disk_dir(tmp_dir.path())
        .ttl(Duration::from_secs(1))
        .strict_deserialization(true)
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
    let cache: RedbCache<u32, SerializeFailsAfterDeserialize> =
        RedbCache::builder("ser-error-struct-variant")
            .disk_dir(tmp_dir.path())
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
/// must return the underlying decode error. The downcast below pins current
/// behavior only — the concrete source type is non-contract (see the semver
/// note on the error type) and may change with the codec in a major release.
#[test]
fn cache_deserialization_error_source_is_wired() {
    use std::error::Error;
    let tmp_dir = temp_dir!();
    // Use strict mode so we get an error from cache_get.
    let cache: RedbCache<u32, u32> = RedbCache::builder("deser-source-wired")
        .disk_dir(tmp_dir.path())
        .strict_deserialization(true)
        .build()
        .expect("error building disk cache");
    raw_insert(&cache, &TEST_KEY.to_string(), vec![0xc1, 0xc1, 0xc1]);

    let err = cache
        .cache_get(&TEST_KEY)
        .expect_err("expected a decode error");
    let source = err
        .source()
        .expect("CacheDeserialization must expose its inner error via source()");
    assert!(
        source.downcast_ref::<rmp_serde::decode::Error>().is_some(),
        "boxed source must downcast to rmp_serde::decode::Error"
    );
}

/// `RedbCacheBuildError::Storage`'s `source` field is wired as the
/// `std::error::Error::source()` of the wrapper. The downcast below pins
/// current behavior only — the concrete source type is non-contract (see
/// the semver note on the error type) and may change with the storage
/// engine in a major release.
#[test]
fn build_error_storage_source_is_wired() {
    use std::error::Error;
    let inner = redb::Error::Io(std::io::Error::other("synthetic redb io error"));
    let err = RedbCacheBuildError::storage(inner);
    assert!(
        err.source().is_some(),
        "RedbCacheBuildError::Storage must expose its inner error via source()"
    );
    // The inner error must be accessible via downcast.
    assert!(
        err.source()
            .unwrap()
            .downcast_ref::<redb::Error>()
            .is_some(),
        "boxed source must downcast to redb::Error"
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

    /// The cache directory created by `disk_dir(path)` must have
    /// mode 0700 (owner rwx only).
    #[test]
    fn explicit_disk_dir_is_created_with_mode_0700() {
        let parent = temp_dir!();
        // Point to a non-existent subdirectory so create_cache_dir must create it.
        let cache_dir = parent.path().join("sub").join("cache");
        let cache: RedbCache<u32, u32> = RedbCache::builder("perm-dir-explicit")
            .disk_dir(&cache_dir)
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
        let cache: RedbCache<u32, u32> = RedbCache::builder("perm-file")
            .disk_dir(dir.path())
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
    /// `validate_cache_dir` with a `PermissionDenied` error.
    #[test]
    fn symlinked_temp_fallback_dir_is_rejected() {
        let real_dir = temp_dir!();
        let link_dir = temp_dir!();
        let link_path = link_dir.path().join("symlink_cache");
        std::os::unix::fs::symlink(real_dir.path(), &link_path).expect("failed to create symlink");
        // validate_cache_dir must reject a symlink.
        let result = super::super::validate_cache_dir(&link_path);
        assert!(result.is_err(), "validate_cache_dir must reject a symlink");
        let err = result.unwrap_err();
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::PermissionDenied,
            "rejection must be PermissionDenied, got {err}"
        );
    }

    /// A real (non-symlink) temp fallback directory must be accepted by
    /// `validate_cache_dir`.
    #[test]
    fn real_temp_fallback_dir_is_accepted() {
        let dir = temp_dir!();
        // Ensure the directory has mode 0700 (which create_cache_dir produces).
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
            .expect("set_permissions");
        let result = super::super::validate_cache_dir(dir.path());
        assert!(
            result.is_ok(),
            "validate_cache_dir must accept a real 0700 dir: {result:?}"
        );
    }

    /// A world-writable temp fallback directory must be rejected.
    #[test]
    fn world_writable_temp_fallback_dir_is_rejected() {
        let dir = temp_dir!();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o777))
            .expect("set_permissions");
        let result = super::super::validate_cache_dir(dir.path());
        assert!(
            result.is_err(),
            "validate_cache_dir must reject a world-writable dir"
        );
        assert_eq!(
            result.unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
    }

    /// A pre-existing default-path candidate left at 0775 by an earlier
    /// `cached` version (created with the umask-002 user-private-group
    /// default of Debian/Ubuntu) must be self-healed to 0700 and accepted,
    /// not rejected forever.
    #[test]
    fn legacy_group_writable_default_dir_is_healed_to_0700() {
        let parent = temp_dir!();
        let dir = parent.path().join("legacy_cache");
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir(&dir).expect("create_dir");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o775))
            .expect("set_permissions");

        super::super::prepare_default_cache_dir(&dir)
            .expect("a legacy 0775 candidate must be healed, not rejected");
        let mode = std::fs::metadata(&dir).expect("metadata").mode() & 0o777;
        assert_eq!(mode, 0o700, "healed dir must be 0700; got {mode:o}");
    }

    /// A fresh (not yet existing) default-path candidate is created at 0700.
    #[test]
    fn fresh_default_dir_is_created_and_accepted() {
        let parent = temp_dir!();
        let dir = parent.path().join("fresh_cache");
        super::super::prepare_default_cache_dir(&dir).expect("fresh candidate must succeed");
        let mode = std::fs::metadata(&dir).expect("metadata").mode() & 0o777;
        assert_eq!(mode, 0o700, "fresh dir must be 0700; got {mode:o}");
    }

    /// A symlinked default-path candidate is not healed (never chmod through
    /// a symlink); it errors with `PermissionDenied` so the default-path
    /// search falls through to the next candidate.
    #[test]
    fn symlinked_default_dir_is_not_healed() {
        let real_dir = temp_dir!();
        let link_parent = temp_dir!();
        let link_path = link_parent.path().join("symlink_cache");
        std::os::unix::fs::symlink(real_dir.path(), &link_path).expect("symlink");

        let result = super::super::prepare_default_cache_dir(&link_path);
        assert!(result.is_err(), "a symlink candidate must be rejected");
        assert_eq!(
            result.unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
    }
}

/// REDB-9: mixed formats across reopen, borrowed writes, refresh, replacement and removal.
#[cfg(feature = "compression_zstd")]
#[test]
fn compression_redb_round_trip_and_maintenance() {
    use crate::SerializeCached;

    let dir = temp_dir!();
    let builder = || RedbCache::<u32, String>::builder("compression").disk_dir(dir.path());
    let large = "repeated payload".repeat(256);
    let small = String::from("small");

    let cache = builder()
        .compression(Compression::Zstd { level: 3 })
        .min_compress_size(256)
        .max_decompressed_size(8192)
        .ttl_secs(60)
        .refresh_on_hit(true)
        .build()
        .unwrap();

    cache.cache_set_ref(&1, &large).unwrap();
    cache.cache_set_ref(&2, &small).unwrap();
    let frame = raw_get(&cache, "1").unwrap();
    assert!(frame.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]));
    assert!(frame.len() < large.len());
    assert_eq!(raw_get(&cache, "2").unwrap()[0], 0x92);
    assert_eq!(cache.cache_get(&1).unwrap().as_ref(), Some(&large));
    assert!(
        raw_get(&cache, "1")
            .unwrap()
            .starts_with(&[0x28, 0xb5, 0x2f, 0xfd])
    );
    assert_eq!(
        cache.cache_set(1, large.clone()).unwrap().as_ref(),
        Some(&large)
    );
    assert_eq!(cache.cache_remove(&1).unwrap().as_ref(), Some(&large));
    cache.cache_set(1, large.clone()).unwrap();

    let old = CachedDiskValue {
        value: large.clone(),
        created_at: SystemTime::UNIX_EPOCH + Duration::from_secs(1),
    };
    raw_insert(
        &cache,
        "3",
        serialize_disk_value(&old, cache.compression).unwrap(),
    );
    assert!(
        raw_get(&cache, "3")
            .unwrap()
            .starts_with(&[0x28, 0xb5, 0x2f, 0xfd])
    );
    let stats = cache.scan_stats().unwrap();
    assert_eq!((stats.entries, stats.expired, stats.undecodable), (3, 1, 0));
    assert_eq!(cache.remove_expired_entries().unwrap(), 1);
    assert!(raw_get(&cache, "3").is_none());
    raw_insert(
        &cache,
        "3",
        serialize_disk_value(&old, cache.compression).unwrap(),
    );
    assert_eq!(cache.cache_get(&3).unwrap(), None);
    assert!(raw_get(&cache, "3").is_none());
    drop(cache);

    let cache = builder().build().unwrap();
    assert_eq!(cache.cache_get(&2).unwrap(), Some(small));
    assert_eq!(cache.cache_remove_entry(&1).unwrap(), Some((1, large)));
}

#[cfg(feature = "compression_zstd")]
#[test]
fn compression_redb_frame_errors_follow_store_policy() {
    let dir = temp_dir!();
    let builder = || {
        RedbCache::<u32, String>::builder("compression-errors")
            .disk_dir(dir.path())
            .ttl_secs(60)
            .max_decompressed_size(512)
    };
    let cache = builder().strict_deserialization(true).build().unwrap();

    let invalid_message_pack = zstd::bulk::compress(&[0xc1], 3).unwrap();
    let corrupt_frame = vec![0x28, 0xb5, 0x2f, 0xfd];
    for (key, bytes) in [(1, invalid_message_pack), (2, corrupt_frame)] {
        raw_insert(&cache, &key.to_string(), bytes.clone());
        let error = cache.cache_get(&key).unwrap_err();
        assert!(
            matches!(error, RedbCacheError::CacheDeserialization { cached_value, .. } if cached_value == bytes)
        );
        assert!(raw_get(&cache, &key.to_string()).is_some());
    }
    assert_eq!(cache.scan_stats().unwrap().undecodable, 2);
    assert!(
        cache
            .remove_expired_entries()
            .unwrap_err()
            .is_deserialization()
    );
    drop(cache);

    let cache = builder().build().unwrap();
    assert_eq!(cache.cache_get(&1).unwrap(), None);
    assert!(raw_get(&cache, "1").is_none());
    assert_eq!(cache.remove_expired_entries().unwrap(), 1);
    assert_eq!(cache.scan_stats().unwrap().entries, 0);
}

#[cfg(all(feature = "compression_zstd", feature = "async"))]
#[tokio::test]
async fn compression_redb_async_operations() {
    use crate::{ConcurrentCachedAsync, SerializeCachedAsync};

    let dir = temp_dir!();
    let cache = RedbCache::<u32, String>::builder("compression-async")
        .disk_dir(dir.path())
        .compression(Compression::Zstd { level: 3 })
        .ttl_secs(60)
        .build()
        .unwrap();
    let large = "async payload".repeat(256);
    cache.async_cache_set_ref(&1, &large).await.unwrap();
    assert!(
        raw_get(&cache, "1")
            .unwrap()
            .starts_with(&[0x28, 0xb5, 0x2f, 0xfd])
    );
    assert_eq!(
        cache
            .async_cache_set(1, large.clone())
            .await
            .unwrap()
            .as_ref(),
        Some(&large)
    );
    assert_eq!(
        cache.async_cache_get(&1).await.unwrap().as_ref(),
        Some(&large)
    );
    assert_eq!(cache.async_scan_stats().await.unwrap().undecodable, 0);
    assert_eq!(cache.async_remove_expired_entries().await.unwrap(), 0);
    assert_eq!(
        cache.async_cache_remove(&1).await.unwrap().as_ref(),
        Some(&large)
    );
    cache.async_cache_set(1, large.clone()).await.unwrap();
    assert_eq!(
        cache.async_cache_remove_entry(&1).await.unwrap(),
        Some((1, large))
    );
}

#[cfg(all(feature = "compression_zstd", feature = "async"))]
#[tokio::test]
async fn compression_allocation_failure_preserves_redb_entries() {
    use crate::ConcurrentCachedAsync;

    let dir = temp_dir!();
    let mut cache = RedbCache::<u32, String>::builder("compression-allocation")
        .disk_dir(dir.path())
        .compression(Compression::Zstd { level: 3 })
        .ttl_secs(60)
        .build()
        .unwrap();
    let value = "repeated payload".repeat(256);
    for key in 1..=4 {
        cache.cache_set(key, value.clone()).unwrap();
    }

    cache.compression.fail_decode_allocation = true;
    assert!(cache.cache_get(&1).unwrap_err().is_deserialization());
    assert!(
        cache
            .async_cache_get(&2)
            .await
            .unwrap_err()
            .is_deserialization()
    );
    assert!(cache.scan_stats().unwrap_err().is_deserialization());
    assert!(
        cache
            .async_scan_stats()
            .await
            .unwrap_err()
            .is_deserialization()
    );
    assert!(
        cache
            .remove_expired_entries()
            .unwrap_err()
            .is_deserialization()
    );
    assert!(
        cache
            .async_remove_expired_entries()
            .await
            .unwrap_err()
            .is_deserialization()
    );
    cache.compression.fail_decode_allocation = false;

    assert_eq!(cache.scan_stats().unwrap().entries, 4);
    for key in 1..=4 {
        assert_eq!(cache.cache_get(&key).unwrap().as_ref(), Some(&value));
    }
}
