//! Async counterparts of the redb read-then-write race regression tests.
//!
//! The async `ConcurrentCachedAsync` impl for `RedbCache` routes every operation
//! through the *same* `disk_cache_get` / `disk_cache_set` helpers as the sync impl,
//! run on a background thread via `blocking::unblock`. The blocking pool can run
//! several of those operations concurrently, so the same read-then-write races the
//! sync tests target are reachable through the async surface. These tests exercise
//! that surface directly so a regression on the async path (e.g. a future rewrite
//! that stops sharing the fixed helpers) is caught.
//!
//! Gated on both `redb_store` and `async`; compiles to nothing otherwise. Run with
//! `cargo test --features redb_store,async`.

#![cfg(all(feature = "redb_store", feature = "async"))]

use std::sync::Arc;

use cached::time::Duration;
use cached::{ConcurrentCached, ConcurrentCachedAsync, RedbCache};
use tempfile::TempDir;

fn build(
    name: &str,
    dir: &TempDir,
    ttl: Option<Duration>,
    refresh: bool,
) -> Arc<RedbCache<u32, u32>> {
    let mut b = RedbCache::<u32, u32>::builder(name)
        .disk_directory(dir.path())
        // No fsync for speed; these are in-process races.
        .durable(false)
        .refresh_on_hit(refresh);
    if let Some(t) = ttl {
        b = b.ttl(t);
    }
    Arc::new(b.build().expect("cache build"))
}

// ── Async test 1: refresh-on-hit does not clobber a concurrent write ─────────
//
// Async mirror of `refresh_does_not_clobber_concurrent_write`. The writer task
// drives key=1 to 1..=N via `async_cache_set`; the reader task hammers
// `async_cache_get` (which triggers refresh every hit, TTL is long so nothing
// expires). Pre-fix, a reader refresh could write a stale value back over the
// writer's newer commit; post-fix the refresh always operates on the current
// authoritative entry. Final value must be N.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn async_refresh_does_not_clobber_concurrent_write() {
    const N: u32 = 3_000;
    let dir = TempDir::new().unwrap();
    let cache = build(
        "async-race-refresh",
        &dir,
        Some(Duration::from_secs(30)),
        true,
    );

    cache.cache_set(1, 0).unwrap();

    let cw = cache.clone();
    let writer = tokio::spawn(async move {
        for v in 1..=N {
            cw.async_cache_set(1, v).await.unwrap();
        }
    });

    let cr = cache.clone();
    let reader = tokio::spawn(async move {
        for _ in 0..N * 2 {
            let _ = cr.async_cache_get(&1).await;
        }
    });

    writer.await.unwrap();
    reader.await.unwrap();

    let got = cache.async_cache_get(&1).await.unwrap();
    assert_eq!(
        got,
        Some(N),
        "async refresh must not resurrect a stale value: expected Some({N}), got {got:?}"
    );
}

// ── Async test 2: cache_get eviction does not delete a freshly-rewritten entry ─
//
// Async mirror of `cache_get_eviction_does_not_delete_fresh_rewrite`. Each round
// pre-expires key=1, then concurrently runs the evicting `async_cache_get` and a
// fresh `async_cache_set`. Pre-fix, the eviction's separate write txn could delete
// the writer's fresh entry; post-fix the write-txn re-check sees it fresh and
// keeps it. Value must be Some(1) after every round.
//
// A `tokio::sync::Barrier` releases both tasks together to keep the race window
// open; the round count makes the ~0.5^ROUNDS miss probability negligible.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn async_cache_get_eviction_does_not_delete_fresh_rewrite() {
    // See the sync counterpart: TTL must comfortably exceed the gap between the
    // writer committing value=1 and the final read, so value=1 cannot legitimately
    // expire before that read (which would be a false failure).
    const TTL: Duration = Duration::from_millis(30);
    const ROUNDS: usize = 200;

    let dir = TempDir::new().unwrap();
    let cache = build("async-race-evict-get", &dir, Some(TTL), false);

    for round in 0..ROUNDS {
        cache.cache_set(1, 0).unwrap();
        tokio::time::sleep(TTL + Duration::from_millis(6)).await;

        let gate = Arc::new(tokio::sync::Barrier::new(2));

        let cr = cache.clone();
        let gr = gate.clone();
        let reader = tokio::spawn(async move {
            gr.wait().await;
            cr.async_cache_get(&1).await
        });

        let cw = cache.clone();
        let gw = gate.clone();
        let writer = tokio::spawn(async move {
            gw.wait().await;
            cw.async_cache_set(1, 1).await
        });

        reader.await.unwrap().unwrap();
        writer.await.unwrap().unwrap();

        let got = cache.async_cache_get(&1).await.unwrap();
        assert_eq!(
            got,
            Some(1),
            "round {round}: async cache_get eviction deleted the writer's fresh \
             entry; expected Some(1), got {got:?}"
        );
    }
}

// ── async_remove_expired_entries ─────────────────────────────────────────────
//
// The async sweep runs the same TTL sweep as the sync `remove_expired_entries`
// on a background thread. It must remove entries whose TTL elapsed, count them,
// and leave fresh entries in place.
#[tokio::test]
async fn async_remove_expired_entries_sweeps_and_counts() {
    const TTL: Duration = Duration::from_millis(50);
    let dir = TempDir::new().unwrap();
    let cache = build("async-remove-expired", &dir, Some(TTL), false);

    // Two entries that will expire.
    cache.async_cache_set(10, 100).await.unwrap();
    cache.async_cache_set(20, 200).await.unwrap();
    tokio::time::sleep(TTL + Duration::from_millis(20)).await;

    // One fresh entry inserted after the sleep.
    cache.async_cache_set(30, 300).await.unwrap();

    let removed = cache.async_remove_expired_entries().await.unwrap();
    assert_eq!(removed, 2, "expected exactly 2 expired entries removed");

    assert_eq!(
        cache.async_cache_get(&30).await.unwrap(),
        Some(300),
        "the fresh entry must survive the async sweep"
    );
    assert_eq!(cache.async_cache_get(&10).await.unwrap(), None);
    assert_eq!(cache.async_cache_get(&20).await.unwrap(), None);
}

// No TTL configured: the async sweep removes nothing and returns 0.
#[tokio::test]
async fn async_remove_expired_entries_no_ttl_returns_zero() {
    let dir = TempDir::new().unwrap();
    let cache = build("async-remove-expired-no-ttl", &dir, None, false);

    cache.async_cache_set(1, 1).await.unwrap();
    let removed = cache.async_remove_expired_entries().await.unwrap();
    assert_eq!(removed, 0, "no TTL means no expirations");
    assert_eq!(cache.async_cache_get(&1).await.unwrap(), Some(1));
}
