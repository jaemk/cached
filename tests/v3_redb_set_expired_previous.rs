//! `RedbCache::cache_set` must return the previous **live** value only.
//!
//! The [`ConcurrentCached::cache_set`] contract says a displaced entry that has
//! already expired is not returned: it is filtered to `None`, so a returned
//! `Some(v)` is always a value that was live for this operation. Every other
//! store honours that (the sharded TTL stores filter explicitly; a Redis key
//! that expired server-side simply GETs nil), but `RedbCache` used to decode the
//! displaced bytes and hand back `cached.value` with no TTL comparison, leaking
//! a stale value out of `cache_set` — a value the caller would then treat as a
//! live previous entry (writing it to a downstream system, counting it as a hit,
//! resurrecting it) when it was in fact garbage that should have been evicted.
//! Its sibling `disk_cache_remove` already filtered correctly, so `cache_set`
//! and `cache_remove` disagreed about the same expired entry.
//!
//! Pre-fix these assertions see `Some(100)` where the contract requires `None`.
//!
//! Gated on `redb_store` like every other redb test; the parity test against
//! `ShardedTtlCache` additionally needs `time_stores`. Run with
//! `cargo test --features redb_store,time_stores`.

#![cfg(feature = "redb_store")]

use std::path::Path;

use cached::time::Duration;
use cached::{ConcurrentCached, RedbCache};
use tempfile::TempDir;

/// Short enough to keep the tests fast, long enough that the write between
/// setup and assertion cannot legitimately outlive it on a loaded runner.
const TTL: Duration = Duration::from_millis(120);
/// Slept past `TTL` before the displacing write, with margin for scheduler jitter.
const PAST_TTL: Duration = Duration::from_millis(220);

/// Scratch databases live in the repo's gitignored `local/` directory rather
/// than the system temp dir. `TempDir` still removes the directory on drop, so
/// nothing is left behind.
fn scratch_dir() -> TempDir {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("local");
    std::fs::create_dir_all(&root).expect("create local/ scratch root");
    TempDir::new_in(root).expect("create scratch dir")
}

fn build(name: &str, dir: &TempDir, ttl: Option<Duration>) -> RedbCache<u32, u32> {
    let mut b = RedbCache::<u32, u32>::builder(name)
        .disk_dir(dir.path())
        // No fsync: these are single-process, in-process assertions.
        .durable(false);
    if let Some(ttl) = ttl {
        b = b.ttl(ttl);
    }
    b.build().expect("cache build")
}

// ── The bug: an expired displaced value was returned ─────────────────────────

#[test]
fn cache_set_over_expired_entry_returns_none() {
    let dir = scratch_dir();
    let cache = build("set-expired-prev", &dir, Some(TTL));

    assert_eq!(
        cache.cache_set(1, 100).unwrap(),
        None,
        "no previous entry exists on the first write"
    );
    std::thread::sleep(PAST_TTL);

    // key=1 is now expired. Displacing it must NOT hand the stale value back.
    let previous = cache.cache_set(1, 200).unwrap();
    assert_eq!(
        previous, None,
        "cache_set must not return an expired displaced value; got {previous:?}"
    );

    // The write itself still took effect.
    assert_eq!(cache.cache_get(&1).unwrap(), Some(200));
}

#[test]
fn cache_set_over_live_entry_still_returns_previous() {
    let dir = scratch_dir();
    // A long TTL so the displaced entry is unambiguously live.
    let cache = build("set-live-prev", &dir, Some(Duration::from_secs(300)));

    cache.cache_set(1, 100).unwrap();
    assert_eq!(
        cache.cache_set(1, 200).unwrap(),
        Some(100),
        "a live displaced value must still be returned"
    );
}

#[test]
fn cache_set_without_ttl_returns_previous_regardless_of_age() {
    let dir = scratch_dir();
    // No TTL configured: nothing ever expires, so age is irrelevant and the
    // displaced value is always returned. Guards against the fix over-filtering.
    let cache = build("set-no-ttl-prev", &dir, None);

    cache.cache_set(1, 100).unwrap();
    std::thread::sleep(PAST_TTL);
    assert_eq!(
        cache.cache_set(1, 200).unwrap(),
        Some(100),
        "with no TTL configured every displaced value is live"
    );
}

#[test]
fn cache_set_agrees_with_cache_remove_on_an_expired_entry() {
    // `cache_remove` already filtered the expired entry to `None`. The two
    // sibling helpers must agree about the same entry rather than one leaking
    // the stale value while the other hides it.
    let dir = scratch_dir();
    let cache = build("set-remove-agree", &dir, Some(TTL));

    cache.cache_set(1, 100).unwrap();
    cache.cache_set(2, 100).unwrap();
    std::thread::sleep(PAST_TTL);

    let removed = cache.cache_remove(&1).unwrap();
    let displaced = cache.cache_set(2, 200).unwrap();
    assert_eq!(
        removed, None,
        "baseline: cache_remove filters the expired entry"
    );
    assert_eq!(
        displaced, removed,
        "cache_set must filter an expired displaced entry exactly like cache_remove; \
         cache_set gave {displaced:?}, cache_remove gave {removed:?}"
    );
}

// ── Parity against the in-memory reference implementation ────────────────────

/// `ShardedTtlCache` is the reference for the `cache_set` contract: it filters a
/// displaced expired entry to `None` (and routes it to `on_evict` instead). The
/// redb store must be indistinguishable from it on this path.
#[cfg(feature = "time_stores")]
#[test]
fn cache_set_expired_previous_matches_sharded_ttl_reference() {
    use cached::ShardedTtlCache;

    let dir = scratch_dir();
    let redb = build("set-expired-parity", &dir, Some(TTL));
    let reference: ShardedTtlCache<u32, u32> = ShardedTtlCache::builder()
        .ttl(TTL)
        .build()
        .expect("reference cache build");

    redb.cache_set(1, 100).unwrap();
    reference.cache_set(1, 100).unwrap();
    std::thread::sleep(PAST_TTL);

    let redb_previous = redb.cache_set(1, 200).unwrap();
    let reference_previous = reference.cache_set(1, 200).unwrap();

    assert_eq!(
        reference_previous, None,
        "reference store must filter the expired displaced value"
    );
    assert_eq!(
        redb_previous, reference_previous,
        "RedbCache disagrees with the ShardedTtlCache reference on a displaced \
         expired entry: redb gave {redb_previous:?}, reference gave {reference_previous:?}"
    );

    // Both stores agree on the new value too.
    assert_eq!(redb.cache_get(&1).unwrap(), Some(200));
    assert_eq!(reference.cache_get(&1).unwrap(), Some(200));
}

// ── The async surface shares the same helper ─────────────────────────────────

/// `async_cache_set` routes through the same `disk_cache_set` helper, so it must
/// filter identically. This catches a future rewrite that stops sharing it.
#[cfg(feature = "async")]
#[tokio::test]
async fn async_cache_set_over_expired_entry_returns_none() {
    use cached::ConcurrentCachedAsync;

    let dir = scratch_dir();
    let cache = build("async-set-expired-prev", &dir, Some(TTL));

    cache.async_cache_set(1, 100).await.unwrap();
    tokio::time::sleep(PAST_TTL).await;

    let previous = cache.async_cache_set(1, 200).await.unwrap();
    assert_eq!(
        previous, None,
        "async_cache_set must not return an expired displaced value; got {previous:?}"
    );
    assert_eq!(cache.async_cache_get(&1).await.unwrap(), Some(200));
}
