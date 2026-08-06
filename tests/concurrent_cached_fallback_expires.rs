/*!
Behavioral coverage for `#[concurrent_cached(expires = true, result_fallback = true)]`.

`#[concurrent_cached]` used to reject `result_fallback` alongside `expires`, so the
combination is newly accepted and every test in this file fails to compile without the
change. The combination works because `ShardedExpiringCache` and `ShardedExpiringLruCache`
both implement `ConcurrentCloneCached`, which supplies the
`cache_get_with_expiry_status` / `cache_peek_with_expiry_status` reads the
`result_fallback` codegen performs - the same reads the sharded TTL stores supply. This
matches `#[cached]`, which has always accepted `expires` as a `result_fallback` store.

Covers:
- the unbounded per-value-expiry store (`ShardedExpiringCache`): an `Err` recompute over
  an expired entry returns the last cached `Ok`
- the size-bounded per-value-expiry store (`ShardedExpiringLruCache`, `max_size`): same
- a fresh (unexpired) entry short-circuits before the body runs, so the fallback path is
  not reachable while the value is live
- `force_refresh` bypassing a live entry still recovers the stale `Ok` on `Err`
- the async expansion of the same combination
*/

#![cfg(feature = "proc_macro")]

use std::sync::atomic::{AtomicUsize, Ordering};

use cached::Expires;
use cached::macros::concurrent_cached;

/// A cached value whose expiry is carried by the value itself, as `expires = true`
/// requires. `expired` is fixed at construction so a test can hand the cache a value
/// that is already stale and force the next call to recompute.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Stamped {
    generation: usize,
    expired: bool,
}

impl Expires for Stamped {
    fn is_expired(&self) -> bool {
        self.expired
    }
}

// ── unbounded per-value expiry: ShardedExpiringCache ──────────────────────────

static UNBOUNDED_CALLS: AtomicUsize = AtomicUsize::new(0);

/// Call 1 stores an already-expired `Ok`; every later call returns `Err`. Without the
/// fallback the second call would surface that `Err`.
#[concurrent_cached(expires = true, result_fallback = true)]
fn expiring_fallback(key: u32) -> Result<Stamped, String> {
    let n = UNBOUNDED_CALLS.fetch_add(1, Ordering::SeqCst);
    let _ = key;
    if n == 0 {
        Ok(Stamped {
            generation: 1,
            expired: true,
        })
    } else {
        Err("refresh failed".to_string())
    }
}

#[test]
fn expires_result_fallback_serves_last_ok_on_err() {
    UNBOUNDED_CALLS.store(0, Ordering::SeqCst);

    let first = expiring_fallback(1).expect("first call computes and caches an `Ok`");
    assert_eq!(first.generation, 1);
    assert_eq!(UNBOUNDED_CALLS.load(Ordering::SeqCst), 1);

    // The stored value reports itself expired, so the cached entry is stale and the body
    // runs again - this time returning `Err`. `result_fallback` substitutes the stale
    // `Ok` instead of propagating the error.
    let second = expiring_fallback(1).expect("`Err` refresh falls back to the last cached `Ok`");
    assert_eq!(
        second.generation, 1,
        "fallback returns the value cached by the first call"
    );
    assert_eq!(
        UNBOUNDED_CALLS.load(Ordering::SeqCst),
        2,
        "the expired entry is recomputed rather than served as a hit"
    );

    // The fallback re-caches the stale value, so the behavior repeats rather than
    // degrading to `Err` on the third call.
    let third = expiring_fallback(1).expect("fallback keeps serving the last cached `Ok`");
    assert_eq!(third.generation, 1);
    assert_eq!(UNBOUNDED_CALLS.load(Ordering::SeqCst), 3);

    // A different key has nothing to fall back to, so its `Err` propagates.
    assert!(
        expiring_fallback(2).is_err(),
        "a key with no cached `Ok` propagates the error"
    );
}

// ── live entries short-circuit before the body runs ───────────────────────────

static LIVE_CALLS: AtomicUsize = AtomicUsize::new(0);

/// The stored value never expires, so after the first call the cache always hits.
#[concurrent_cached(expires = true, result_fallback = true)]
fn expiring_fallback_live(key: u32) -> Result<Stamped, String> {
    let n = LIVE_CALLS.fetch_add(1, Ordering::SeqCst);
    let _ = key;
    if n == 0 {
        Ok(Stamped {
            generation: 1,
            expired: false,
        })
    } else {
        Err("should not be reached".to_string())
    }
}

#[test]
fn expires_result_fallback_hits_live_entry_without_running_body() {
    LIVE_CALLS.store(0, Ordering::SeqCst);

    assert_eq!(expiring_fallback_live(7).unwrap().generation, 1);
    assert_eq!(expiring_fallback_live(7).unwrap().generation, 1);
    assert_eq!(expiring_fallback_live(7).unwrap().generation, 1);
    assert_eq!(
        LIVE_CALLS.load(Ordering::SeqCst),
        1,
        "an unexpired entry is returned from the cache; the body runs once"
    );
}

// ── size-bounded per-value expiry: ShardedExpiringLruCache ────────────────────

static LRU_CALLS: AtomicUsize = AtomicUsize::new(0);

#[concurrent_cached(expires = true, result_fallback = true, max_size = 8)]
fn expiring_lru_fallback(key: u32) -> Result<Stamped, String> {
    let n = LRU_CALLS.fetch_add(1, Ordering::SeqCst);
    let _ = key;
    if n == 0 {
        Ok(Stamped {
            generation: 42,
            expired: true,
        })
    } else {
        Err("refresh failed".to_string())
    }
}

#[test]
fn expires_max_size_result_fallback_serves_last_ok_on_err() {
    LRU_CALLS.store(0, Ordering::SeqCst);

    assert_eq!(expiring_lru_fallback(3).unwrap().generation, 42);
    let stale =
        expiring_lru_fallback(3).expect("`Err` refresh falls back on the LRU expiring store");
    assert_eq!(stale.generation, 42);
    assert_eq!(LRU_CALLS.load(Ordering::SeqCst), 2);
}

// ── force_refresh over a live entry still recovers the stale Ok ───────────────

static FORCED_CALLS: AtomicUsize = AtomicUsize::new(0);

/// `bypass` is kept out of the cache key via `key`/`convert` so both calls address the
/// same entry; the flag only drives the `force_refresh` predicate.
#[concurrent_cached(
    expires = true,
    result_fallback = true,
    key = "u32",
    convert = "{ key }",
    force_refresh = "{ bypass }"
)]
fn expiring_forced_fallback(key: u32, bypass: bool) -> Result<Stamped, String> {
    let _ = bypass; // consumed by the generated force_refresh guard, not the body
    let _ = key;
    let n = FORCED_CALLS.fetch_add(1, Ordering::SeqCst);
    if n == 0 {
        Ok(Stamped {
            generation: 9,
            expired: false,
        })
    } else {
        Err("refresh failed".to_string())
    }
}

#[test]
fn expires_result_fallback_with_force_refresh_recovers_stale_ok() {
    FORCED_CALLS.store(0, Ordering::SeqCst);

    assert_eq!(
        expiring_forced_fallback(9, false).unwrap().generation,
        9,
        "first call computes and caches"
    );
    // Bypassing a *live* entry re-runs the body; the `Err` falls back to the value the
    // bypassed peek captured.
    assert_eq!(
        expiring_forced_fallback(9, true).unwrap().generation,
        9,
        "force_refresh bypass still falls back to the cached `Ok`"
    );
    assert_eq!(FORCED_CALLS.load(Ordering::SeqCst), 2);
}

// ── async expansion of the same combination ───────────────────────────────────

#[cfg(feature = "async")]
mod async_tests {
    use super::*;

    static ASYNC_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[concurrent_cached(expires = true, result_fallback = true)]
    async fn expiring_fallback_async(key: u32) -> Result<Stamped, String> {
        let n = ASYNC_CALLS.fetch_add(1, Ordering::SeqCst);
        let _ = key;
        if n == 0 {
            Ok(Stamped {
                generation: 5,
                expired: true,
            })
        } else {
            Err("refresh failed".to_string())
        }
    }

    #[tokio::test]
    async fn async_expires_result_fallback_serves_last_ok_on_err() {
        ASYNC_CALLS.store(0, Ordering::SeqCst);

        assert_eq!(expiring_fallback_async(1).await.unwrap().generation, 5);
        let stale = expiring_fallback_async(1)
            .await
            .expect("`Err` refresh falls back to the last cached `Ok`");
        assert_eq!(stale.generation, 5);
        assert_eq!(ASYNC_CALLS.load(Ordering::SeqCst), 2);
    }
}
