/*!
Golden tests for the frozen on-wire formats of the IO stores.

The format-stability sections on `RedisCache` and `RedbCache` are a compatibility
contract: entries written by any 3.x release stay readable by every later 3.x
release. Both stores serialize with `rmp_serde::to_vec` (never `to_vec_named`),
so values are POSITIONAL MessagePack arrays -- the field names never reach the
wire and the field ORDER is what a reader depends on. These tests read the bytes
the stores actually wrote, so reordering a field of the stored envelope, or
switching a write site to a named encoding, fails here instead of silently
corrupting existing caches.

Also covered: the Redis KEY layout. The value envelope's `version` field lives
inside the value, so it cannot describe the key layout -- a key-layout change is
invisible to a reader, which simply never finds the old entries. The key tests
pin the escaped layout and the agreement between the key a cache writes and the
`SCAN MATCH` pattern `cache_clear` scans.

The redb tests need no server. The redis tests gate on the `redis_store` feature
and skip cleanly when `CACHED_REDIS_CONNECTION_STRING` is unset.
*/

#[cfg(feature = "redb_store")]
mod redb_value_layout {
    use cached::{ConcurrentCached, RedbCache};
    use redb::{Database, ReadableDatabase, TableDefinition};
    use tempfile::TempDir;

    /// The redb table name is part of the frozen on-disk format, so the test
    /// names it independently instead of reusing a crate-internal constant.
    const TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("cached_disk_cache");

    /// Read the raw stored bytes for `key` straight out of the redb file.
    /// The cache must already be dropped: redb holds an exclusive file lock.
    fn raw_entry_bytes(path: &std::path::Path, key: &str) -> Vec<u8> {
        let db = Database::open(path).expect("open the redb file written by RedbCache");
        let rtxn = db.begin_read().expect("begin read txn");
        let table = rtxn
            .open_table(TABLE)
            .expect("the frozen table name must exist in the file");
        let guard = table
            .get(key)
            .expect("table get")
            .expect("the entry written through the store must be present");
        guard.value().to_vec()
    }

    /// A stored entry is a positional 2-element array of `value` then
    /// `created_at`, and `created_at` is itself a positional array of
    /// seconds-since-epoch then sub-second nanoseconds.
    #[test]
    fn stored_entry_is_a_positional_array_of_value_then_created_at() {
        let dir = TempDir::new().expect("temp dir");
        let disk_path = {
            let cache: RedbCache<String, String> = RedbCache::builder("frozen_format_golden")
                .disk_dir(dir.path())
                .build()
                .expect("build RedbCache");
            cache
                .cache_set("k".to_string(), "hi".to_string())
                .expect("cache_set");
            cache.disk_path().to_path_buf()
        };

        let raw = raw_entry_bytes(&disk_path, "k");

        // 0x92: 2-element fixarray. A named encoding would start with 0x82
        // (fixmap) and carry the strings "value"/"created_at".
        // 0xa2 "hi": 2-byte fixstr, so the FIRST field is the value.
        // 0x92: the nested 2-element array of the `created_at` timestamp.
        assert_eq!(
            &raw[..5],
            &[0x92, 0xa2, b'h', b'i', 0x92],
            "frozen layout: positional array of `value` then `created_at`, got {raw:02x?}"
        );

        // The whole entry decodes as a positional tuple in that order.
        let (value, (secs, nanos)): (String, (u64, u32)) =
            rmp_serde::from_slice(&raw).expect("stored entry must decode positionally");
        assert_eq!(value, "hi", "the value must be the first field");
        assert!(nanos < 1_000_000_000, "nanos field out of range: {nanos}");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_secs();
        assert!(
            secs <= now && now - secs < 600,
            "the second field must be the write timestamp: got {secs}, now {now}"
        );
    }

    /// The borrowed setter (`SerializeCached::cache_set_ref`) writes the same
    /// layout as the owned one: both go through the same positional encoding.
    #[test]
    fn cache_set_ref_writes_the_same_layout() {
        use cached::SerializeCached;

        let dir = TempDir::new().expect("temp dir");
        let disk_path = {
            let cache: RedbCache<String, String> = RedbCache::builder("frozen_format_golden_ref")
                .disk_dir(dir.path())
                .build()
                .expect("build RedbCache");
            cache
                .cache_set_ref(&"k".to_string(), &"hi".to_string())
                .expect("cache_set_ref");
            cache.disk_path().to_path_buf()
        };

        let raw = raw_entry_bytes(&disk_path, "k");
        assert_eq!(
            &raw[..5],
            &[0x92, 0xa2, b'h', b'i', 0x92],
            "cache_set_ref must write the frozen positional layout, got {raw:02x?}"
        );
    }
}

#[cfg(feature = "redis_store")]
mod redis_value_layout {
    use cached::{ConcurrentCached, RedisCache};
    use std::time::Duration;

    const ENV_KEY: &str = "CACHED_REDIS_CONNECTION_STRING";

    macro_rules! skip_without_redis {
        () => {
            if std::env::var(ENV_KEY).is_err() {
                return;
            }
        };
    }

    fn raw_connection(cache: &RedisCache<String, String>) -> redis::Connection {
        redis::Client::open(cache.connection_string().reveal())
            .expect("raw client")
            .get_connection()
            .expect("raw connection")
    }

    /// The stored envelope is a positional 2-element array of `value` then
    /// `version`. These are the exact bytes a 3.x release must keep writing.
    #[test]
    fn stored_envelope_is_a_positional_array_of_value_then_version() {
        skip_without_redis!();

        let prefix = "frozen_format_golden_value";
        let cache = RedisCache::<String, String>::builder(prefix)
            .namespace("")
            .ttl(Duration::from_secs(60))
            .build()
            .expect("build RedisCache");
        cache.cache_clear().expect("clear");

        cache
            .cache_set("golden".to_string(), "hello".to_string())
            .expect("cache_set");

        let mut raw = raw_connection(&cache);
        let bytes: Vec<u8> = redis::cmd("GET")
            .arg(format!(":{prefix}:golden"))
            .query(&mut raw)
            .expect("raw GET of the stored envelope");

        // 0x92: 2-element fixarray (a named encoding would start 0x82, fixmap).
        // 0xa5 "hello": 5-byte fixstr, so the FIRST field is the value.
        // 0x01: version 1, the SECOND field.
        assert_eq!(
            bytes,
            vec![0x92, 0xa5, b'h', b'e', b'l', b'l', b'o', 0x01],
            "frozen layout: positional array of `value` then `version`, got {bytes:02x?}"
        );

        // The same bytes decode as a positional tuple in that order.
        let (value, version): (String, Option<u64>) =
            rmp_serde::from_slice(&bytes).expect("stored envelope must decode positionally");
        assert_eq!(value, "hello", "the value must be the first field");
        assert_eq!(version, Some(1), "the version must be the second field");

        cache.cache_clear().expect("clean up");
    }
}

#[cfg(feature = "redis_store")]
mod redis_key_layout {
    use cached::{ConcurrentCached, RedisCache};
    use std::time::Duration;

    const ENV_KEY: &str = "CACHED_REDIS_CONNECTION_STRING";

    macro_rules! skip_without_redis {
        () => {
            if std::env::var(ENV_KEY).is_err() {
                return;
            }
        };
    }

    fn build(namespace: &str, prefix: &str) -> RedisCache<String, String> {
        RedisCache::<String, String>::builder(prefix)
            .namespace(namespace)
            .ttl(Duration::from_secs(60))
            .build()
            .expect("build RedisCache")
    }

    fn key_exists(cache: &RedisCache<String, String>, key: &str) -> bool {
        let mut raw = redis::Client::open(cache.connection_string().reveal())
            .expect("raw client")
            .get_connection()
            .expect("raw connection");
        redis::cmd("EXISTS")
            .arg(key)
            .query::<i64>(&mut raw)
            .expect("raw EXISTS")
            == 1
    }

    /// The key a cache writes is `{namespace}:{prefix}:{key}` with `:` and `%`
    /// percent-escaped in every field, and with fixed arity: every key carries
    /// both separators, so an empty namespace shows up as an empty leading field.
    #[test]
    fn written_keys_use_the_escaped_layout() {
        skip_without_redis!();

        let plain = build("", "frozen_key_plain");
        plain.cache_clear().expect("clear");
        plain
            .cache_set("k".to_string(), "v".to_string())
            .expect("cache_set");
        assert!(
            key_exists(&plain, ":frozen_key_plain:k"),
            "a namespace-less cache writes `:{{prefix}}:{{key}}`, keeping both separators"
        );
        assert!(
            !key_exists(&plain, "frozen_key_plain:k"),
            "the namespace field must not be dropped when it is empty"
        );

        let escaped = build("a:b", "frozen_key_escaped");
        escaped.cache_clear().expect("clear");
        escaped
            .cache_set("x:y".to_string(), "v".to_string())
            .expect("cache_set");
        assert!(
            key_exists(&escaped, "a%3Ab:frozen_key_escaped:x%3Ay"),
            "separators inside a field are percent-escaped"
        );
        assert!(
            !key_exists(&escaped, "a:b:frozen_key_escaped:x:y"),
            "the unescaped pre-3.0 key must no longer be written"
        );

        plain.cache_clear().expect("clean up");
        escaped.cache_clear().expect("clean up");
    }

    /// Two caches whose namespace/prefix split the same characters differently
    /// used to write to the identical key, so each overwrote the other's entries.
    /// Escaping makes the (namespace, prefix, key) mapping injective.
    #[test]
    fn differently_split_namespace_and_prefix_do_not_collide() {
        skip_without_redis!();

        let left = build("a:b", "frozen_key_split");
        let right = build("a", "b:frozen_key_split");
        left.cache_clear().expect("clear left");
        right.cache_clear().expect("clear right");

        left.cache_set("k".to_string(), "left".to_string())
            .expect("set left");
        right
            .cache_set("k".to_string(), "right".to_string())
            .expect("set right");

        assert_eq!(
            left.cache_get(&"k".to_string()).expect("get left"),
            Some("left".to_string()),
            "the second cache must not have overwritten the first cache's entry"
        );
        assert_eq!(
            right.cache_get(&"k".to_string()).expect("get right"),
            Some("right".to_string())
        );

        left.cache_clear().expect("clean up left");
        right.cache_clear().expect("clean up right");
    }

    /// `cache_clear` scans a glob built from the same escaped fields as the keys,
    /// so it deletes exactly this cache's entries: all of its own (the pattern
    /// still covers escaped keys) and none of a neighbouring cache's (the
    /// neighbour's keys fall outside the escaped scope).
    #[test]
    fn clear_covers_own_escaped_keys_and_spares_the_neighbour() {
        skip_without_redis!();

        let left = build("a:b", "frozen_key_clear");
        let right = build("a", "b:frozen_key_clear");
        left.cache_clear().expect("clear left");
        right.cache_clear().expect("clear right");

        left.cache_set("k:1".to_string(), "left".to_string())
            .expect("set left");
        right
            .cache_set("k:1".to_string(), "right".to_string())
            .expect("set right");

        left.cache_clear().expect("clear left");

        assert_eq!(
            left.cache_get(&"k:1".to_string()).expect("get left"),
            None,
            "cache_clear must match this cache's own escaped keys"
        );
        assert_eq!(
            right.cache_get(&"k:1".to_string()).expect("get right"),
            Some("right".to_string()),
            "cache_clear must not reach a differently-split neighbouring cache"
        );

        right.cache_clear().expect("clean up right");
    }

    /// Glob metacharacters in a field are escaped for the scan on top of the
    /// percent-escaping, so a cache whose namespace contains both still clears
    /// its own entries and only its own.
    #[test]
    fn clear_handles_glob_metacharacters_and_separators_together() {
        skip_without_redis!();

        let starred = build("n*s:x", "frozen_key_glob");
        let neighbour = build("nzs:x", "frozen_key_glob");
        starred.cache_clear().expect("clear starred");
        neighbour.cache_clear().expect("clear neighbour");

        starred
            .cache_set("k".to_string(), "starred".to_string())
            .expect("set starred");
        neighbour
            .cache_set("k".to_string(), "neighbour".to_string())
            .expect("set neighbour");
        assert!(
            key_exists(&starred, "n*s%3Ax:frozen_key_glob:k"),
            "the metacharacter is literal in the key; only the scan pattern quotes it"
        );

        starred.cache_clear().expect("clear starred");

        assert_eq!(
            starred.cache_get(&"k".to_string()).expect("get starred"),
            None,
            "cache_clear must match this cache's own keys through both escapes"
        );
        assert_eq!(
            neighbour
                .cache_get(&"k".to_string())
                .expect("get neighbour"),
            Some("neighbour".to_string()),
            "an unescaped `*` in the scan pattern would have deleted the neighbour"
        );

        neighbour.cache_clear().expect("clean up neighbour");
    }
}
