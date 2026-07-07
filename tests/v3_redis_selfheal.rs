//! Regression tests for the redis self-heal GET-then-DEL race (C6).
//!
//! On a deserialization failure in non-strict mode, `cache_get` self-heals by
//! deleting the offending entry. The pre-fix code issued an unconditional `DEL`,
//! so a concurrent `PSETEX`/`SET` of a valid value that committed between the
//! self-heal `GET` and the `DEL` was silently deleted. The fix replaces the
//! unconditional `DEL` with a Lua conditional delete that removes the key only
//! if its current value still equals the corrupt bytes the self-heal read, so a
//! concurrent valid write is never clobbered.
//!
//! These tests require a live redis. When no server is reachable the cache build
//! fails and the test skips (returns early) — CI runs them against a real redis.

#![cfg(feature = "redis_store")]

use cached::time::Duration;

// Undecodable bytes: not valid MessagePack and not the legacy JSON fallback
// either, so `deserialize_cached_redis_value` fails and drives the self-heal.
const CORRUPT: &[u8] = b"\xff\xff not a valid cached value \x00\x01\x02";

// ── sync ─────────────────────────────────────────────────────────────────────

mod sync_tests {
    use super::*;
    use cached::{ConcurrentCached, RedisCache};
    use std::sync::{Arc, Barrier};

    fn try_build(prefix: &str) -> Option<RedisCache<String, String>> {
        RedisCache::<String, String>::builder(prefix)
            .ttl(Duration::from_secs(30))
            .namespace("")
            .build()
            .ok()
    }

    fn raw_conn(cache: &RedisCache<String, String>) -> redis::Connection {
        redis::Client::open(cache.connection_string().reveal())
            .expect("open redis client")
            .get_connection()
            .expect("redis connection")
    }

    // Deterministic baseline: a self-heal on an entry that is STILL corrupt at
    // delete time removes it (the conditional delete matches and fires).
    #[test]
    fn self_heal_deletes_still_corrupt_entry() {
        let Some(cache) = try_build("v3_selfheal_sync_del") else {
            eprintln!("skipping self_heal_deletes_still_corrupt_entry: no live redis");
            return;
        };
        let mut conn = raw_conn(&cache);
        let key = "k".to_string();
        let full_key = "v3_selfheal_sync_del:k";

        let _: () = redis::cmd("SET")
            .arg(full_key)
            .arg(CORRUPT)
            .query(&mut conn)
            .unwrap();

        // Non-strict self-heal: undecodable entry becomes a cache miss and is
        // removed (its current value still equals the corrupt bytes read).
        assert_eq!(cache.cache_get(&key).unwrap(), None);

        let exists: bool = redis::cmd("EXISTS").arg(full_key).query(&mut conn).unwrap();
        assert!(!exists, "still-corrupt entry must be deleted by self-heal");
    }

    // The race: a concurrent valid write between the self-heal GET and DEL must
    // survive. Pre-fix (unconditional DEL) deletes it; post-fix (conditional
    // DEL) leaves it in place because the current value no longer matches the
    // corrupt bytes the self-heal read.
    #[test]
    fn self_heal_does_not_clobber_concurrent_write() {
        const ROUNDS: usize = 300;
        let Some(cache) = try_build("v3_selfheal_sync_race") else {
            eprintln!("skipping self_heal_does_not_clobber_concurrent_write: no live redis");
            return;
        };
        let cache = Arc::new(cache);
        let mut conn = raw_conn(&cache);
        let key = "racekey".to_string();
        let full_key = "v3_selfheal_sync_race:racekey";

        for round in 0..ROUNDS {
            // Inject a corrupt entry for this round.
            let _: () = redis::cmd("SET")
                .arg(full_key)
                .arg(CORRUPT)
                .query(&mut conn)
                .unwrap();

            let gate = Arc::new(Barrier::new(2));

            let cr = cache.clone();
            let gr = gate.clone();
            let k1 = key.clone();
            let reader = std::thread::spawn(move || {
                gr.wait();
                let _ = cr.cache_get(&k1); // self-heal path
            });

            let cw = cache.clone();
            let gw = gate.clone();
            let k2 = key.clone();
            let writer = std::thread::spawn(move || {
                gw.wait();
                cw.cache_set(k2, "valid".to_string())
            });

            reader.join().unwrap();
            writer.join().unwrap().unwrap();

            let got = cache.cache_get(&key).unwrap();
            assert_eq!(
                got,
                Some("valid".to_string()),
                "round {round}: self-heal DEL clobbered the concurrent valid write"
            );
        }

        let _: () = redis::cmd("DEL").arg(full_key).query(&mut conn).unwrap();
    }
}

// ── async ────────────────────────────────────────────────────────────────────

#[cfg(feature = "redis_tokio")]
mod async_tests {
    use super::*;
    use cached::{AsyncRedisCache, ConcurrentCachedAsync};
    use std::sync::Arc;
    use tokio::sync::Barrier;

    async fn try_build(prefix: &str) -> Option<AsyncRedisCache<String, String>> {
        AsyncRedisCache::<String, String>::builder(prefix)
            .ttl(Duration::from_secs(30))
            .namespace("")
            .build()
            .await
            .ok()
    }

    fn raw_conn(cache: &AsyncRedisCache<String, String>) -> redis::Connection {
        redis::Client::open(cache.connection_string().reveal())
            .expect("open redis client")
            .get_connection()
            .expect("redis connection")
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn self_heal_does_not_clobber_concurrent_write_async() {
        const ROUNDS: usize = 300;
        let Some(cache) = try_build("v3_selfheal_async_race").await else {
            eprintln!("skipping self_heal_does_not_clobber_concurrent_write_async: no live redis");
            return;
        };
        let cache = Arc::new(cache);
        let mut conn = raw_conn(&cache);
        let key = "racekey".to_string();
        let full_key = "v3_selfheal_async_race:racekey";

        for round in 0..ROUNDS {
            let _: () = redis::cmd("SET")
                .arg(full_key)
                .arg(CORRUPT)
                .query(&mut conn)
                .unwrap();

            let gate = Arc::new(Barrier::new(2));

            let cr = cache.clone();
            let gr = gate.clone();
            let k1 = key.clone();
            let reader = tokio::spawn(async move {
                gr.wait().await;
                let _ = cr.async_cache_get(&k1).await; // self-heal path
            });

            let cw = cache.clone();
            let gw = gate.clone();
            let k2 = key.clone();
            let writer = tokio::spawn(async move {
                gw.wait().await;
                cw.async_cache_set(k2, "valid".to_string()).await
            });

            reader.await.unwrap();
            writer.await.unwrap().unwrap();

            let got = cache.async_cache_get(&key).await.unwrap();
            assert_eq!(
                got,
                Some("valid".to_string()),
                "round {round}: async self-heal DEL clobbered the concurrent valid write"
            );
        }

        let _: () = redis::cmd("DEL").arg(full_key).query(&mut conn).unwrap();
    }
}
