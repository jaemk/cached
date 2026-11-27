use crate::time::Duration;
use std::thread::sleep;

use super::*;

fn now_millis() -> u128 {
    crate::time::SystemTime::now()
        .duration_since(crate::time::UNIX_EPOCH)
        .unwrap()
        .as_millis()
}

/// The raw Redis key a default-namespace cache with `prefix` writes for
/// `key`. Built through the store's own key builder because the test
/// prefixes carry a separator, which the key layout escapes.
fn raw_key(prefix: &str, key: &str) -> String {
    super::generate_redis_key(super::DEFAULT_NAMESPACE, prefix, key)
}

#[test]
fn redis_cache() {
    let c: RedisCache<u32, u32> = RedisCache::builder(format!("{}:redis-cache-test", now_millis()))
        .ttl(Duration::from_secs(2))
        .namespace("in-tests:")
        .build()
        .unwrap();

    assert!(c.cache_get(&1).unwrap().is_none());

    assert!(c.cache_set(1, 100).unwrap().is_none());
    assert!(c.cache_get(&1).unwrap().is_some());

    // 500ms past the 2s ttl. `Duration::new`'s second argument is nanoseconds, so the
    // literal 500_000 this used to pass left a 500us margin between a local sleep and
    // redis's own expiry clock, which is not enough under load.
    sleep(Duration::from_millis(2_500));
    assert!(c.cache_get(&1).unwrap().is_none());

    let old = ConcurrentCacheTtl::set_ttl(&c, Duration::from_secs(1)).unwrap();
    assert_eq!(2, old.as_secs());
    assert!(c.cache_set(1, 100).unwrap().is_none());
    assert!(c.cache_get(&1).unwrap().is_some());

    // 600ms past the 1s ttl; see the note above.
    sleep(Duration::from_millis(1_600));
    assert!(c.cache_get(&1).unwrap().is_none());

    ConcurrentCacheTtl::set_ttl(&c, Duration::from_secs(10)).unwrap();
    assert!(c.cache_set(1, 100).unwrap().is_none());
    assert!(c.cache_set(2, 100).unwrap().is_none());
    assert_eq!(c.cache_get(&1).unwrap().unwrap(), 100);
    assert_eq!(c.cache_get(&1).unwrap().unwrap(), 100);
}

#[test]
fn remove() {
    let c: RedisCache<u32, u32> =
        RedisCache::builder(format!("{}:redis-cache-test-remove", now_millis()))
            .ttl(Duration::from_secs(3600))
            .build()
            .unwrap();

    assert!(c.cache_set(1, 100).unwrap().is_none());
    assert!(c.cache_set(2, 200).unwrap().is_none());
    assert!(c.cache_set(3, 300).unwrap().is_none());

    assert_eq!(100, c.cache_remove(&1).unwrap().unwrap());
}

/// D2: default mode -- a corrupt cache entry is deleted and `cache_get`
/// returns `Ok(None)` (self-heal) instead of propagating the decode error.
#[test]
fn cache_get_self_heals_corrupted_entry_by_default() {
    let mut conn = redis::Client::open("redis://127.0.0.1:6399")
        .unwrap()
        .get_connection()
        .unwrap();
    let prefix = format!("{}:selfheal-default", now_millis());
    let key = raw_key(&prefix, "1");
    // Write garbage bytes so deserialization will fail.
    let _: () = redis::cmd("SET")
        .arg(&key)
        .arg(b"\xff\xfe\xfd".as_ref())
        .query(&mut conn)
        .unwrap();

    let c: RedisCache<u32, u32> = RedisCache::builder(prefix)
        .ttl(Duration::from_secs(3600))
        .build()
        .unwrap();

    // In default mode (strict_deserialization=false), corrupt entry returns Ok(None)
    let result = c.cache_get(&1).unwrap();
    assert!(
        result.is_none(),
        "expected Ok(None) after self-heal, got: {result:?}"
    );

    // The key must have been deleted (self-heal).
    let exists: bool = redis::cmd("EXISTS").arg(&key).query(&mut conn).unwrap();
    assert!(!exists, "corrupt key must be deleted after self-heal");
}

/// D2: strict mode -- a corrupt cache entry returns `Err(CacheDeserialization)`
/// and the key is NOT deleted.
#[test]
fn cache_get_strict_mode_returns_error_for_corrupted_entry() {
    let mut conn = redis::Client::open("redis://127.0.0.1:6399")
        .unwrap()
        .get_connection()
        .unwrap();
    let prefix = format!("{}:selfheal-strict", now_millis());
    let key = raw_key(&prefix, "1");
    let _: () = redis::cmd("SET")
        .arg(&key)
        .arg(b"\xff\xfe\xfd".as_ref())
        .query(&mut conn)
        .unwrap();

    let c: RedisCache<u32, u32> = RedisCache::builder(prefix)
        .ttl(Duration::from_secs(3600))
        .strict_deserialization(true)
        .build()
        .unwrap();

    // In strict mode, corrupt entry must return a deserialization error.
    let err = c
        .cache_get(&1)
        .expect_err("strict mode must return Err for corrupt entry");
    assert!(
        err.is_deserialization(),
        "expected CacheDeserialization, got: {err:?}"
    );

    // The key must NOT have been deleted in strict mode.
    let exists: bool = redis::cmd("EXISTS").arg(&key).query(&mut conn).unwrap();
    assert!(exists, "corrupt key must NOT be deleted in strict mode");

    // cleanup
    let _: () = redis::cmd("DEL").arg(&key).query(&mut conn).unwrap();
}

/// REDIS-10: on the SET path, a pre-existing corrupt value returns Ok(None)
/// not an error, in both default and strict modes.
#[test]
fn cache_set_displaced_corrupt_previous_returns_ok_none() {
    let mut conn = redis::Client::open("redis://127.0.0.1:6399")
        .unwrap()
        .get_connection()
        .unwrap();
    let prefix = format!("{}:redis10-test", now_millis());
    let key = raw_key(&prefix, "1");
    // Plant a corrupt value so the GET in the SET pipeline will see garbage.
    let _: () = redis::cmd("SET")
        .arg(&key)
        .arg(b"\xff\xfe\xfd".as_ref())
        .query(&mut conn)
        .unwrap();

    let c: RedisCache<u32, u32> = RedisCache::builder(prefix)
        .ttl(Duration::from_secs(3600))
        .build()
        .unwrap();

    // cache_set must succeed and return Ok(None) even though the displaced value
    // was corrupt.
    let result = c.cache_set(1, 42);
    assert!(
        result.is_ok(),
        "cache_set must not error on corrupt displaced value; got: {result:?}"
    );
    assert!(
        result.unwrap().is_none(),
        "displaced corrupt value must yield Ok(None)"
    );
}

/// D2: after a self-healed miss the corrupt key is deleted; recomputing via
/// `cache_set` and reading again must produce a HIT. Covers the full
/// self-heal -> recompute -> hit cycle, not just the initial `Ok(None)`.
#[test]
fn cache_get_self_heal_then_recompute_produces_hit() {
    let mut conn = redis::Client::open("redis://127.0.0.1:6399")
        .unwrap()
        .get_connection()
        .unwrap();
    let prefix = format!("{}:selfheal-recompute", now_millis());
    let key = raw_key(&prefix, "1");
    let _: () = redis::cmd("SET")
        .arg(&key)
        .arg(b"\xff\xfe\xfd".as_ref())
        .query(&mut conn)
        .unwrap();

    let c: RedisCache<u32, u32> = RedisCache::builder(prefix)
        .ttl(Duration::from_secs(3600))
        .build()
        .unwrap();

    // Corrupt entry self-heals to a miss and is deleted.
    assert_eq!(c.cache_get(&1).unwrap(), None, "self-heal returns a miss");
    let exists: bool = redis::cmd("EXISTS").arg(&key).query(&mut conn).unwrap();
    assert!(!exists, "self-heal must delete the corrupt key");

    // Recompute over the healed miss (previous value is None), then a HIT.
    assert_eq!(
        c.cache_set(1, 77).unwrap(),
        None,
        "recompute writes over the healed miss"
    );
    assert_eq!(
        c.cache_get(&1).unwrap(),
        Some(77),
        "the read after recompute is a HIT"
    );

    let _: () = redis::cmd("DEL").arg(&key).query(&mut conn).unwrap();
}

/// `cache_set_ref` returns `Ok(())` regardless of any pre-existing (even
/// corrupt) value under the key: it does not read the previous value back, so
/// an undecodable displaced value can never surface as an error. The new value
/// is written and readable.
#[test]
fn cache_set_ref_over_corrupt_previous_returns_ok_unit() {
    use crate::SerializeCached;
    let mut conn = redis::Client::open("redis://127.0.0.1:6399")
        .unwrap()
        .get_connection()
        .unwrap();
    let prefix = format!("{}:redis10-setref", now_millis());
    let key = raw_key(&prefix, "1");
    let _: () = redis::cmd("SET")
        .arg(&key)
        .arg(b"\xff\xfe\xfd".as_ref())
        .query(&mut conn)
        .unwrap();

    let c: RedisCache<u32, u32> = RedisCache::builder(prefix)
        .ttl(Duration::from_secs(3600))
        .build()
        .unwrap();

    let val = 42u32;
    let result = SerializeCached::cache_set_ref(&c, &1, &val);
    assert!(
        result.is_ok(),
        "cache_set_ref must not error over a corrupt previous value; got: {result:?}"
    );
    // The new value was written and is now readable.
    assert_eq!(c.cache_get(&1).unwrap(), Some(42));

    let _: () = redis::cmd("DEL").arg(&key).query(&mut conn).unwrap();
}

/// BUG-2 (sync, default mode): `cache_remove` on a key with corrupt stored bytes
/// returns `Ok(None)` and the key is deleted. The corrupt bytes must NOT surface
/// as `Err(CacheDeserialization)` in default (non-strict) mode.
#[test]
fn cache_remove_corrupt_default_mode_returns_ok_none() {
    let mut conn = redis::Client::open("redis://127.0.0.1:6399")
        .unwrap()
        .get_connection()
        .unwrap();
    let prefix = format!("{}:remove-corrupt-default", now_millis());
    let key = raw_key(&prefix, "1");
    // Plant garbage bytes so deserialization will fail.
    let _: () = redis::cmd("SET")
        .arg(&key)
        .arg(b"\xff\xfe\xfd".as_ref())
        .query(&mut conn)
        .unwrap();

    let c: RedisCache<u32, u32> = RedisCache::builder(prefix)
        .ttl(Duration::from_secs(3600))
        .build()
        .unwrap();

    // Default mode: corrupt entry must be silently discarded as Ok(None).
    let result = c.cache_remove(&1);
    assert!(
        result.is_ok(),
        "cache_remove must not error in default mode on corrupt entry; got: {result:?}"
    );
    assert!(
        result.unwrap().is_none(),
        "cache_remove must return Ok(None) for corrupt entry in default mode"
    );

    // The key must be gone after remove regardless of decode failure.
    let exists: bool = redis::cmd("EXISTS").arg(&key).query(&mut conn).unwrap();
    assert!(
        !exists,
        "cache_remove must delete the key even when bytes are corrupt"
    );
}

/// BUG-2 (sync, strict mode): `cache_remove` on a key with corrupt stored bytes
/// returns `Err(CacheDeserialization)` in strict mode AND the key is still deleted
/// (the pipeline already ran GET+DEL atomically).
#[test]
fn cache_remove_corrupt_strict_mode_returns_error_and_key_is_gone() {
    let mut conn = redis::Client::open("redis://127.0.0.1:6399")
        .unwrap()
        .get_connection()
        .unwrap();
    let prefix = format!("{}:remove-corrupt-strict", now_millis());
    let key = raw_key(&prefix, "1");
    let _: () = redis::cmd("SET")
        .arg(&key)
        .arg(b"\xff\xfe\xfd".as_ref())
        .query(&mut conn)
        .unwrap();

    let c: RedisCache<u32, u32> = RedisCache::builder(prefix)
        .ttl(Duration::from_secs(3600))
        .strict_deserialization(true)
        .build()
        .unwrap();

    // Strict mode: must return a deserialization error.
    let err = c
        .cache_remove(&1)
        .expect_err("strict cache_remove must return Err for corrupt entry");
    assert!(
        err.is_deserialization(),
        "expected CacheDeserialization error, got: {err:?}"
    );

    // The key must still be gone -- the GET+DEL pipeline already ran.
    let exists: bool = redis::cmd("EXISTS").arg(&key).query(&mut conn).unwrap();
    assert!(
        !exists,
        "key must be deleted even when strict cache_remove errors"
    );
}

/// BUG-2 (sync, happy path): `cache_remove` on a VALID stored value returns
/// `Ok(Some(value))` and the key is actually gone afterward (a follow-up get
/// returns None). Locks the non-regressed remove-and-return contract next to
/// the corrupt-path tests. The pre-existing `remove` test asserts the returned
/// value but never verifies the key was deleted.
#[test]
fn cache_remove_valid_value_returns_some_and_deletes_key() {
    let mut conn = redis::Client::open("redis://127.0.0.1:6399")
        .unwrap()
        .get_connection()
        .unwrap();
    let prefix = format!("{}:remove-valid", now_millis());
    let key = raw_key(&prefix, "1");

    let c: RedisCache<u32, u32> = RedisCache::builder(prefix)
        .ttl(Duration::from_secs(3600))
        .build()
        .unwrap();

    assert!(c.cache_set(1, 100).unwrap().is_none());
    assert_eq!(
        c.cache_remove(&1).unwrap(),
        Some(100),
        "cache_remove must return the stored value"
    );
    // The key is physically gone...
    let exists: bool = redis::cmd("EXISTS").arg(&key).query(&mut conn).unwrap();
    assert!(!exists, "cache_remove must delete the key");
    // ...and a follow-up get is a miss.
    assert_eq!(
        c.cache_get(&1).unwrap(),
        None,
        "get after remove must be a miss"
    );
}

/// BUG-2 (sync, missing key): `cache_remove` on a key that was never stored
/// returns `Ok(None)` (the `res.0 == None` branch), not an error.
#[test]
fn cache_remove_missing_key_returns_ok_none() {
    let prefix = format!("{}:remove-missing", now_millis());
    let c: RedisCache<u32, u32> = RedisCache::builder(prefix)
        .ttl(Duration::from_secs(3600))
        .build()
        .unwrap();

    let result = c.cache_remove(&12345);
    assert!(
        result.is_ok(),
        "cache_remove on a missing key must not error; got: {result:?}"
    );
    assert_eq!(
        result.unwrap(),
        None,
        "cache_remove on a missing key must return Ok(None)"
    );
}

/// BUG-2 (sync `cache_remove_entry` delegation, valid value): the entry variant
/// must return `Ok(Some((key, value)))` and delete the key. Pins that
/// `cache_remove_entry` forwards the key alongside the removed value.
#[test]
fn cache_remove_entry_valid_returns_key_and_value() {
    let mut conn = redis::Client::open("redis://127.0.0.1:6399")
        .unwrap()
        .get_connection()
        .unwrap();
    let prefix = format!("{}:remove-entry-valid", now_millis());
    let key = raw_key(&prefix, "7");

    let c: RedisCache<u32, u32> = RedisCache::builder(prefix)
        .ttl(Duration::from_secs(3600))
        .build()
        .unwrap();

    assert!(c.cache_set(7, 700).unwrap().is_none());
    assert_eq!(
        c.cache_remove_entry(&7).unwrap(),
        Some((7, 700)),
        "cache_remove_entry must return the key and the stored value"
    );
    let exists: bool = redis::cmd("EXISTS").arg(&key).query(&mut conn).unwrap();
    assert!(!exists, "cache_remove_entry must delete the key");
}

/// BUG-2 (sync `cache_remove_entry` delegation, default mode): corrupt bytes
/// must be discarded as `Ok(None)` (the entry delegates to `cache_remove`, so it
/// inherits the non-strict self-heal) and the key is deleted. Direct coverage —
/// the delegation was previously only inferred.
#[test]
fn cache_remove_entry_corrupt_default_mode_returns_ok_none() {
    let mut conn = redis::Client::open("redis://127.0.0.1:6399")
        .unwrap()
        .get_connection()
        .unwrap();
    let prefix = format!("{}:remove-entry-corrupt-default", now_millis());
    let key = raw_key(&prefix, "1");
    let _: () = redis::cmd("SET")
        .arg(&key)
        .arg(b"\xff\xfe\xfd".as_ref())
        .query(&mut conn)
        .unwrap();

    let c: RedisCache<u32, u32> = RedisCache::builder(prefix)
        .ttl(Duration::from_secs(3600))
        .build()
        .unwrap();

    let result = c.cache_remove_entry(&1);
    assert!(
        result.is_ok(),
        "cache_remove_entry must not error in default mode on corrupt entry; got: {result:?}"
    );
    assert_eq!(
        result.unwrap(),
        None,
        "cache_remove_entry must return Ok(None) for a corrupt entry in default mode"
    );
    let exists: bool = redis::cmd("EXISTS").arg(&key).query(&mut conn).unwrap();
    assert!(
        !exists,
        "cache_remove_entry must delete the key even when bytes are corrupt"
    );
}

/// BUG-2 (sync `cache_remove_entry` delegation, strict mode): corrupt bytes must
/// surface as the exact `RedisCacheError::CacheDeserialization { .. }` variant
/// (not merely any Err) and the key is still deleted. Direct coverage of the
/// strict delegation, and the only place the exact variant is matched by shape.
#[test]
fn cache_remove_entry_corrupt_strict_mode_returns_error_and_key_is_gone() {
    let mut conn = redis::Client::open("redis://127.0.0.1:6399")
        .unwrap()
        .get_connection()
        .unwrap();
    let prefix = format!("{}:remove-entry-corrupt-strict", now_millis());
    let key = raw_key(&prefix, "1");
    let _: () = redis::cmd("SET")
        .arg(&key)
        .arg(b"\xff\xfe\xfd".as_ref())
        .query(&mut conn)
        .unwrap();

    let c: RedisCache<u32, u32> = RedisCache::builder(prefix)
        .ttl(Duration::from_secs(3600))
        .strict_deserialization(true)
        .build()
        .unwrap();

    let err = c
        .cache_remove_entry(&1)
        .expect_err("strict cache_remove_entry must return Err for corrupt entry");
    assert!(
        matches!(err, RedisCacheError::CacheDeserialization { .. }),
        "expected the exact CacheDeserialization variant, got: {err:?}"
    );
    let exists: bool = redis::cmd("EXISTS").arg(&key).query(&mut conn).unwrap();
    assert!(
        !exists,
        "key must be deleted even when strict cache_remove_entry errors"
    );
}

/// REDIS-8: both Redis formats survive enabling and disabling compressed writes.
#[cfg(feature = "compression_zstd")]
#[test]
fn compression_redis_mixed_values_and_frame_errors() {
    use crate::SerializeCached;

    let url = "redis://127.0.0.1:6399";
    let mut conn = redis::Client::open(url).unwrap().get_connection().unwrap();
    let prefix = format!("{}:compression", now_millis());
    let builder = || RedisCache::<u32, String>::builder(&prefix).connection_string(url);
    let raw = builder().build().unwrap();
    let compressed = builder()
        .compression(Compression::Zstd { level: 3 })
        .min_compress_size(256)
        .max_decompressed_size(8192)
        .build()
        .unwrap()
        .clone();
    let strict = builder()
        .strict_deserialization(true)
        .max_decompressed_size(512)
        .build()
        .unwrap();
    let large = "repeated payload".repeat(256);
    let small = String::from("small");

    raw.cache_set(1, large.clone()).unwrap();
    assert_eq!(compressed.cache_get(&1).unwrap().as_ref(), Some(&large));
    compressed.cache_set_ref(&1, &large).unwrap();
    compressed.cache_set(2, small.clone()).unwrap();

    let read_bytes = |key, conn: &mut redis::Connection| -> Option<Vec<u8>> {
        redis::cmd("GET")
            .arg(raw_key(&prefix, key))
            .query(conn)
            .unwrap()
    };
    let frame = read_bytes("1", &mut conn).unwrap();
    assert!(frame.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]));
    assert!(frame.len() < large.len());
    assert_eq!(read_bytes("2", &mut conn).unwrap()[0], 0x92);
    assert_eq!(raw.cache_get(&1).unwrap().as_ref(), Some(&large));
    assert_eq!(raw.cache_get(&2).unwrap(), Some(small.clone()));
    assert!(matches!(
        strict.cache_get(&1).unwrap_err(),
        RedisCacheError::CacheDeserialization { cached_value, .. } if cached_value == frame
    ));
    assert_eq!(read_bytes("1", &mut conn).unwrap(), frame);
    assert_eq!(
        compressed.cache_set(1, large.clone()).unwrap().as_ref(),
        Some(&large)
    );
    assert_eq!(raw.cache_remove_entry(&1).unwrap(), Some((1, large)));

    let legacy = br#"{"value":"legacy","version":1}"#;
    let bad = [0x28, 0xb5, 0x2f, 0xfd];
    for (key, bytes) in [("3", legacy.as_slice()), ("4", bad.as_slice())] {
        let _: () = redis::cmd("SET")
            .arg(raw_key(&prefix, key))
            .arg(bytes)
            .query(&mut conn)
            .unwrap();
    }
    assert_eq!(
        compressed.cache_remove(&3).unwrap().as_deref(),
        Some("legacy")
    );
    assert_eq!(compressed.cache_get(&4).unwrap(), None);
    assert!(read_bytes("4", &mut conn).is_none());
    raw.cache_clear().unwrap();
}
