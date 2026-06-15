/*!
Regression tests for force_refresh single-evaluation on the unsync_reads path.

Covers:
- force_refresh predicate is evaluated AT MOST ONCE per call when `unsync_reads`
  is set (the `SyncWriteMode::Default` + `unsync_reads` path). Before the fix the
  predicate was expanded inside both the optimistic read-lock block and the
  write-lock re-check, causing double evaluation on every call.
- const-generic functions with `key` + `convert` compile and cache correctly.
*/

#![cfg(feature = "proc_macro")]
// The lifetime-only-generic regression tests below deliberately spell out `<'a>`
// to prove the macro's generic guard accepts a lifetime param (eliding it would
// drop the very thing under test). clippy's `needless_lifetimes` fires on the
// macro-generated origin fn, where an attribute cannot be forwarded, so suppress
// it file-wide.
#![allow(clippy::needless_lifetimes)]

use std::sync::atomic::{AtomicUsize, Ordering};

use cached::macros::cached;

// ── force_refresh single-evaluation on unsync_reads path ──────────────────────
//
// The predicate block is a side-effecting counter increment. If the predicate is
// evaluated more than once per call, the counter will over-count relative to the
// number of function invocations.

static UNSYNC_FR_PREDICATE_EVALS: AtomicUsize = AtomicUsize::new(0);
static UNSYNC_FR_BODY_CALLS: AtomicUsize = AtomicUsize::new(0);

// `bypass` is excluded from the cache key via `key`/`convert` so the same slot
// is hit/refreshed regardless of the flag.
// `unsync_reads` triggers the optimistic read-lock path under `SyncWriteMode::Default`.
// The `force_refresh` predicate increments the counter so we can count evaluations.
#[cached(
    key = "i32",
    convert = "{ x }",
    unsync_reads,
    force_refresh = "{ UNSYNC_FR_PREDICATE_EVALS.fetch_add(1, Ordering::SeqCst); bypass }"
)]
fn unsync_fr_fn(x: i32, bypass: bool) -> i32 {
    let _ = bypass; // consumed by the generated force_refresh guard, not the body
    UNSYNC_FR_BODY_CALLS.fetch_add(1, Ordering::SeqCst);
    x
}

#[test]
fn force_refresh_predicate_evaluated_once_per_call_on_unsync_reads_path() {
    UNSYNC_FR_PREDICATE_EVALS.store(0, Ordering::SeqCst);
    UNSYNC_FR_BODY_CALLS.store(0, Ordering::SeqCst);

    // Call 1: cache miss (bypass = false). Predicate evaluates once -> false.
    // Body runs, result is cached.
    let _ = unsync_fr_fn(42, false);
    assert_eq!(
        UNSYNC_FR_PREDICATE_EVALS.load(Ordering::SeqCst),
        1,
        "call 1 (miss, no bypass): predicate must run exactly once"
    );
    assert_eq!(UNSYNC_FR_BODY_CALLS.load(Ordering::SeqCst), 1);

    // Call 2: cache hit (bypass = false). Predicate evaluates once -> false.
    // Cached value returned, body does NOT run.
    let _ = unsync_fr_fn(42, false);
    assert_eq!(
        UNSYNC_FR_PREDICATE_EVALS.load(Ordering::SeqCst),
        2,
        "call 2 (hit, no bypass): predicate must run exactly once (not twice)"
    );
    assert_eq!(
        UNSYNC_FR_BODY_CALLS.load(Ordering::SeqCst),
        1,
        "hit path: body must not re-run"
    );

    // Call 3: force bypass (bypass = true). Predicate evaluates once -> true.
    // Body re-runs and overwrites the cache entry.
    let _ = unsync_fr_fn(42, true);
    assert_eq!(
        UNSYNC_FR_PREDICATE_EVALS.load(Ordering::SeqCst),
        3,
        "call 3 (bypass): predicate must run exactly once"
    );
    assert_eq!(
        UNSYNC_FR_BODY_CALLS.load(Ordering::SeqCst),
        2,
        "bypass: body must re-run"
    );

    // Verify: total predicate evals == total calls (3), never more.
    // Pre-fix: each call would double-evaluate the predicate (6 total), not 3.
    assert_eq!(
        UNSYNC_FR_PREDICATE_EVALS.load(Ordering::SeqCst),
        3,
        "total predicate evals must equal total calls (1 eval per call)"
    );
}

// ── const-generic positive test: key + convert compiles and caches ─────────────
//
// Confirms that a const-generic function WITH key + convert successfully compiles
// and caches: the first call runs the body and caches the result; subsequent calls
// with the same arguments are served from the cache.
//
// Note: the const parameter N must appear in the argument types so that the
// compiler can infer N when calling the generated `_no_cache` helper. Here we use
// a `&[i32; N]` parameter so N is determined by the slice length at the call site.

static CONST_GEN_CALLS: AtomicUsize = AtomicUsize::new(0);

// `key`/`convert` pins the cache key to a concrete `String` (the slice's debug
// repr), satisfying the guard. The const parameter N is part of the argument type
// `&[i32; N]`, so the compiler can infer N at each call site.
#[cached(key = "String", convert = r#"{ format!("{:?}", arr) }"#)]
fn const_generic_cached<const N: usize>(arr: &[i32; N]) -> usize {
    CONST_GEN_CALLS.fetch_add(1, Ordering::SeqCst);
    arr.len()
}

#[test]
fn const_generic_with_key_convert_compiles_and_caches() {
    CONST_GEN_CALLS.store(0, Ordering::SeqCst);

    let arr = [1i32, 2, 3];

    // First call: miss, body runs.
    let v1 = const_generic_cached(&arr);
    assert_eq!(v1, 3);
    assert_eq!(CONST_GEN_CALLS.load(Ordering::SeqCst), 1);

    // Second call with same key: hit.
    let v2 = const_generic_cached(&arr);
    assert_eq!(v2, 3);
    assert_eq!(
        CONST_GEN_CALLS.load(Ordering::SeqCst),
        1,
        "second call with same key must be a cache hit"
    );
}

// ── lifetime-only generics must NOT be rejected (Change 1 regression guard) ──
//
// The generic-function guard checks `type_params()` and `const_params()` only.
// `syn` excludes lifetime parameters from both iterators, so a function that is
// generic only in lifetimes (no type params, no const params) must compile and
// cache WITHOUT `key`/`convert`. This test pins that behavior.
//
// Regression trigger: if the guard were changed to `generics.params.is_empty()`
// instead of the targeted `type_params()/const_params()` check, every cached fn
// that takes a `&'a T` (e.g. `&str`) would be rejected with a compile error.
//
// These functions use `key`/`convert` to own the borrowed data for the key,
// which is normal for reference args, but the ABSENCE of a rejection is what
// we are pinning: the macro must not complain about the lifetime parameter.

use cached::macros::concurrent_cached;

static LIFETIME_CACHED_CALLS: AtomicUsize = AtomicUsize::new(0);

// A function generic in lifetime only. The default key derives from the owned
// `String` produced by `convert`, pinning the cache slot. Without lifetime
// generics being explicitly allowed by the guard, this would fail to expand.
#[cached(key = "String", convert = r#"{ s.to_owned() }"#)]
fn lifetime_only_cached<'a>(s: &'a str) -> usize {
    LIFETIME_CACHED_CALLS.fetch_add(1, Ordering::SeqCst);
    s.len()
}

#[test]
fn lifetime_only_generic_cached_compiles_and_caches() {
    LIFETIME_CACHED_CALLS.store(0, Ordering::SeqCst);

    // First call: miss, body runs.
    assert_eq!(lifetime_only_cached("hello"), 5);
    assert_eq!(LIFETIME_CACHED_CALLS.load(Ordering::SeqCst), 1);

    // Second call with the same string: cache hit, body does NOT run.
    assert_eq!(lifetime_only_cached("hello"), 5);
    assert_eq!(
        LIFETIME_CACHED_CALLS.load(Ordering::SeqCst),
        1,
        "second call with same arg must be a cache hit (lifetime generic must not be rejected)"
    );

    // Different string: miss, body runs again.
    assert_eq!(lifetime_only_cached("world!"), 6);
    assert_eq!(LIFETIME_CACHED_CALLS.load(Ordering::SeqCst), 2);
}

static LIFETIME_CONC_CALLS: AtomicUsize = AtomicUsize::new(0);

// Same regression guard but for `#[concurrent_cached]`. The concurrent path has
// its own copy of the guard (concurrent_cached.rs ~line 257).
#[concurrent_cached(key = "String", convert = r#"{ s.to_owned() }"#)]
fn lifetime_only_concurrent<'a>(s: &'a str) -> usize {
    LIFETIME_CONC_CALLS.fetch_add(1, Ordering::SeqCst);
    s.len()
}

#[test]
fn lifetime_only_generic_concurrent_cached_compiles_and_caches() {
    LIFETIME_CONC_CALLS.store(0, Ordering::SeqCst);

    // First call: miss, body runs.
    assert_eq!(lifetime_only_concurrent("abc"), 3);
    assert_eq!(LIFETIME_CONC_CALLS.load(Ordering::SeqCst), 1);

    // Second call with the same string: cache hit, body does NOT run.
    assert_eq!(lifetime_only_concurrent("abc"), 3);
    assert_eq!(
        LIFETIME_CONC_CALLS.load(Ordering::SeqCst),
        1,
        "second call with same arg must be a cache hit (lifetime generic must not be rejected)"
    );

    // Different string: miss, body runs again.
    assert_eq!(lifetime_only_concurrent("xy"), 2);
    assert_eq!(LIFETIME_CONC_CALLS.load(Ordering::SeqCst), 2);
}

// ── unsync_reads WITHOUT force_refresh: SyncWriteMode::Default baseline ───────
//
// Pins that the `SyncWriteMode::Default` + `unsync_reads` path (the path that
// received the force_refresh single-eval hoist) correctly caches WITHOUT any
// force_refresh: a miss populates the cache and all subsequent calls are hits.
//
// This is the zero-predicate case for the hoisted binding:
//   `let __cached_force_refreshing = if true { false } else { true };`  -> always false
// Both the read-lock block and the write-lock re-check use `false`, so the cache
// is always consulted and the body runs exactly once per unique key.
//
// Without this test, a future refactor that accidentally sets
// `__cached_force_refreshing = true` unconditionally would silently skip the
// cache on every call, and no v3 test would catch it.

static UNSYNC_NO_FR_CALLS: AtomicUsize = AtomicUsize::new(0);

#[cached(unsync_reads, sync_writes = "default")]
fn unsync_no_force_refresh(x: i32) -> i32 {
    UNSYNC_NO_FR_CALLS.fetch_add(1, Ordering::SeqCst);
    x * 2
}

#[test]
fn unsync_reads_without_force_refresh_caches_correctly() {
    UNSYNC_NO_FR_CALLS.store(0, Ordering::SeqCst);

    // First call: miss, body runs.
    assert_eq!(unsync_no_force_refresh(3), 6);
    assert_eq!(UNSYNC_NO_FR_CALLS.load(Ordering::SeqCst), 1);

    // Second call, same key: hit, body must NOT run.
    assert_eq!(unsync_no_force_refresh(3), 6);
    assert_eq!(
        UNSYNC_NO_FR_CALLS.load(Ordering::SeqCst),
        1,
        "unsync_reads without force_refresh: second call must be a cache hit"
    );

    // Different key: miss, body runs.
    assert_eq!(unsync_no_force_refresh(7), 14);
    assert_eq!(UNSYNC_NO_FR_CALLS.load(Ordering::SeqCst), 2);

    // Repeat different key: hit.
    assert_eq!(unsync_no_force_refresh(7), 14);
    assert_eq!(
        UNSYNC_NO_FR_CALLS.load(Ordering::SeqCst),
        2,
        "unsync_reads without force_refresh: repeated call must stay cached"
    );
}

// ── unsync_reads WITH always-true force_refresh: every call recomputes ────────
//
// Pins that the `SyncWriteMode::Default` + `unsync_reads` + always-bypassing
// force_refresh path recomputes on EVERY call. The hoisted binding evaluates to
// `true` on every call, both the optimistic read-lock block and the write-lock
// re-check skip the cache, and the body runs every time.
//
// Without this test, a bug that short-circuits the hoisted flag to `false`
// (never bypass) would silently serve stale values and go undetected.

static UNSYNC_ALWAYS_FR_CALLS: AtomicUsize = AtomicUsize::new(0);

#[cached(
    key = "i32",
    convert = "{ x }",
    unsync_reads,
    sync_writes = "default",
    force_refresh = "{ true }"
)]
fn unsync_always_force_refresh(x: i32) -> i32 {
    UNSYNC_ALWAYS_FR_CALLS.fetch_add(1, Ordering::SeqCst);
    x
}

#[test]
fn unsync_reads_with_always_true_force_refresh_recomputes_every_call() {
    UNSYNC_ALWAYS_FR_CALLS.store(0, Ordering::SeqCst);

    // Every call bypasses the cache regardless of whether the entry is warm.
    let _ = unsync_always_force_refresh(5);
    let _ = unsync_always_force_refresh(5);
    let _ = unsync_always_force_refresh(5);
    assert_eq!(
        UNSYNC_ALWAYS_FR_CALLS.load(Ordering::SeqCst),
        3,
        "always-bypass predicate: body must run on every call (3 calls -> 3 body executions)"
    );
}

// ── by_key + unsync_reads + force_refresh: single inline guard, no double-eval ─
//
// The `SyncWriteMode::ByKey` + `unsync_reads` path expands `#force_refresh_guard`
// inline into `by_key_cache_get_return_block` once, with no write-lock re-check
// (unlike `SyncWriteMode::Default` + `unsync_reads` which had the double-eval bug).
// After the key-specific lock is acquired, the function body is called
// unconditionally if the guard caused a bypass, then `set_cache_and_return` runs.
// There is intentionally no second guard expansion in the write-lock section.
//
// This test documents and pins the behavior of the by_key + unsync_reads +
// force_refresh combination. It also demonstrates that a side-effecting predicate
// on this path is evaluated exactly once per call (single expansion, no write-lock
// re-check means no double-eval risk on this path).

static BY_KEY_UNSYNC_FR_PREDICATE_EVALS: AtomicUsize = AtomicUsize::new(0);
static BY_KEY_UNSYNC_FR_BODY_CALLS: AtomicUsize = AtomicUsize::new(0);

#[cached(
    key = "i32",
    convert = "{ x }",
    sync_writes = "by_key",
    unsync_reads,
    force_refresh = "{ BY_KEY_UNSYNC_FR_PREDICATE_EVALS.fetch_add(1, Ordering::SeqCst); bypass }"
)]
fn by_key_unsync_force_refresh(x: i32, bypass: bool) -> i32 {
    let _ = bypass; // consumed by the generated force_refresh guard
    BY_KEY_UNSYNC_FR_BODY_CALLS.fetch_add(1, Ordering::SeqCst);
    x
}

#[test]
fn by_key_unsync_reads_force_refresh_single_eval_per_call() {
    BY_KEY_UNSYNC_FR_PREDICATE_EVALS.store(0, Ordering::SeqCst);
    BY_KEY_UNSYNC_FR_BODY_CALLS.store(0, Ordering::SeqCst);

    // Call 1: miss (bypass = false). Predicate evaluates once -> false. Body runs.
    let _ = by_key_unsync_force_refresh(10, false);
    assert_eq!(
        BY_KEY_UNSYNC_FR_PREDICATE_EVALS.load(Ordering::SeqCst),
        1,
        "by_key + unsync: miss, no bypass: predicate must run exactly once"
    );
    assert_eq!(BY_KEY_UNSYNC_FR_BODY_CALLS.load(Ordering::SeqCst), 1);

    // Call 2: hit (bypass = false). Predicate evaluates once -> false.
    // Cache hit returned, body does NOT run.
    let _ = by_key_unsync_force_refresh(10, false);
    assert_eq!(
        BY_KEY_UNSYNC_FR_PREDICATE_EVALS.load(Ordering::SeqCst),
        2,
        "by_key + unsync: hit, no bypass: predicate must run exactly once"
    );
    assert_eq!(
        BY_KEY_UNSYNC_FR_BODY_CALLS.load(Ordering::SeqCst),
        1,
        "hit path: body must not re-run"
    );

    // Call 3: bypass (bypass = true). Predicate evaluates once -> true.
    // Body re-runs.
    let _ = by_key_unsync_force_refresh(10, true);
    assert_eq!(
        BY_KEY_UNSYNC_FR_PREDICATE_EVALS.load(Ordering::SeqCst),
        3,
        "by_key + unsync: bypass: predicate must run exactly once"
    );
    assert_eq!(
        BY_KEY_UNSYNC_FR_BODY_CALLS.load(Ordering::SeqCst),
        2,
        "bypass: body must re-run"
    );

    // Total: 3 calls -> 3 predicate evals. The by_key path has no write-lock
    // re-check, so the predicate is never expanded twice even before the fix.
    assert_eq!(
        BY_KEY_UNSYNC_FR_PREDICATE_EVALS.load(Ordering::SeqCst),
        3,
        "total predicate evals must equal total calls (1 eval per call on by_key path)"
    );
}

// ── force_refresh + result_fallback: stale fallback preserved for LIVE entry ─────
//
// Baseline: with a LIVE (not yet expired) entry, force-refresh + Err still returns
// the stale Ok fallback. This was already correct before the fix; we pin it to
// ensure the fix does not break the live-entry case.
//
// The function uses SyncWriteMode::Disabled (the default, no sync_writes attribute)
// and ttl_secs = 60 so the entry stays live throughout the test.

#[cfg(feature = "time_stores")]
mod result_fallback_live_tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use cached::macros::cached;

    static RF_LIVE_BODY_CALLS: AtomicUsize = AtomicUsize::new(0);
    static RF_LIVE_RETURN_ERR: AtomicBool = AtomicBool::new(false);

    #[cached(
        key = "i32",
        convert = "{ x }",
        ttl_secs = 60,
        result_fallback = true,
        force_refresh = "{ bypass }"
    )]
    fn rf_live_fn(x: i32, bypass: bool) -> Result<i32, String> {
        let _ = bypass; // consumed by the generated force_refresh guard
        RF_LIVE_BODY_CALLS.fetch_add(1, Ordering::SeqCst);
        if RF_LIVE_RETURN_ERR.load(Ordering::SeqCst) {
            Err(format!("error for {x}"))
        } else {
            Ok(x * 10)
        }
    }

    #[test]
    fn result_fallback_force_refresh_live_entry_returns_stale_ok() {
        RF_LIVE_BODY_CALLS.store(0, Ordering::SeqCst);
        RF_LIVE_RETURN_ERR.store(false, Ordering::SeqCst);

        // Seed an Ok value into the cache (bypass = false).
        let first = rf_live_fn(100, false);
        assert_eq!(first, Ok(1000), "seed call must return Ok(1000)");
        assert_eq!(RF_LIVE_BODY_CALLS.load(Ordering::SeqCst), 1);

        // Now the body will return Err.
        RF_LIVE_RETURN_ERR.store(true, Ordering::SeqCst);

        // Force-refresh (bypass = true) with Err recompute over a LIVE entry.
        // result_fallback must return the stale Ok(1000), not the Err.
        let second = rf_live_fn(100, true);
        assert_eq!(
            second,
            Ok(1000),
            "force-refresh Err over a LIVE entry must return stale Ok fallback"
        );
        assert_eq!(RF_LIVE_BODY_CALLS.load(Ordering::SeqCst), 2);
    }
}

// ── force_refresh + result_fallback: stale fallback preserved for EXPIRED entry ──
//
// Regression guard for the bug where `cache_peek` (used on the bypass branch) returns
// `None` for an expired TTL entry, causing the stale `Ok` fallback to be lost when a
// bypassed recompute returns `Err` over an entry that has expired.
//
// Before the fix: the bypass branch called `CachedPeek::cache_peek`, which returns `None`
// for expired entries. An Err recompute over an expired key therefore had no fallback.
// After the fix: the bypass branch calls `CloneCached::cache_peek_with_expiry_status`,
// which returns `(Some(stale_value), true)` for an expired entry, preserving the fallback.
//
// This test FAILS on the pre-fix code path (the Err propagates instead of the stale Ok)
// and PASSES after the fix.
//
// The function uses SyncWriteMode::Disabled (the default, no sync_writes attribute)
// and ttl_secs = 1 so the entry expires quickly.

#[cfg(feature = "time_stores")]
mod result_fallback_expired_tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use cached::macros::cached;

    static RF_EXPIRED_BODY_CALLS: AtomicUsize = AtomicUsize::new(0);
    static RF_EXPIRED_RETURN_ERR: AtomicBool = AtomicBool::new(false);

    // Separate function from the live-entry test above so the cache static is independent
    // (each #[cached] fn has its own static) and the two tests do not share state.
    #[cached(
        key = "i32",
        convert = "{ x }",
        ttl_secs = 1,
        result_fallback = true,
        force_refresh = "{ bypass }"
    )]
    fn rf_expired_fn(x: i32, bypass: bool) -> Result<i32, String> {
        let _ = bypass; // consumed by the generated force_refresh guard
        RF_EXPIRED_BODY_CALLS.fetch_add(1, Ordering::SeqCst);
        if RF_EXPIRED_RETURN_ERR.load(Ordering::SeqCst) {
            Err(format!("error for {x}"))
        } else {
            Ok(x * 10)
        }
    }

    #[test]
    fn result_fallback_force_refresh_expired_entry_returns_stale_ok() {
        RF_EXPIRED_BODY_CALLS.store(0, Ordering::SeqCst);
        RF_EXPIRED_RETURN_ERR.store(false, Ordering::SeqCst);

        // Seed an Ok value into the cache (bypass = false).
        let first = rf_expired_fn(200, false);
        assert_eq!(first, Ok(2000), "seed call must return Ok(2000)");
        assert_eq!(RF_EXPIRED_BODY_CALLS.load(Ordering::SeqCst), 1);

        // Wait for the TTL to expire (ttl_secs = 1 -> sleep at least 1100ms).
        std::thread::sleep(std::time::Duration::from_millis(1200));

        // Now the body will return Err.
        RF_EXPIRED_RETURN_ERR.store(true, Ordering::SeqCst);

        // Force-refresh (bypass = true) with Err recompute over an EXPIRED entry.
        // Pre-fix: cache_peek returns None for expired -> fallback lost -> Err propagated.
        // Post-fix: cache_peek_with_expiry_status returns (Some(2000), true) -> Ok(2000) returned.
        let second = rf_expired_fn(200, true);
        assert_eq!(
            second,
            Ok(2000),
            "force-refresh Err over an EXPIRED entry must return stale Ok fallback (regression)"
        );
        assert_eq!(RF_EXPIRED_BODY_CALLS.load(Ordering::SeqCst), 2);
    }
}

// ── store diversity: expired-entry Err-fallback across every macro-reachable store ─
//
// The author's expired-fallback test exercises only the default `TtlCache`
// (`ttl_secs` alone). The same bug — the bypass branch losing the stale `Ok` over
// an EXPIRED entry — lives in the bypass path regardless of which single-owner TTL
// store the macro selects. These tests drive the OTHER store overrides reachable
// through `#[cached]` attribute combinations:
//
//   ttl_secs + max_size       -> LruTtlCache
//   expires                   -> ExpiringCache  (per-value expiry)
//   expires + max_size        -> ExpiringLruCache
//
// `TtlSortedCache` is not selectable through any `#[cached]` attribute; its override
// is certified directly in `v3_cache_peek_with_expiry_status.rs`.

// LruTtlCache: ttl_secs + max_size.
//
// Each subtest uses its OWN `#[cached]` function so its cache slot and body-call
// counter are fully isolated. The test binary runs tests in parallel by default;
// two tests sharing one cached fn + one global counter would race on the count.
#[cfg(feature = "time_stores")]
mod result_fallback_lru_ttl_tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use cached::macros::cached;

    static RF_LRUTTL_EXP_BODY_CALLS: AtomicUsize = AtomicUsize::new(0);
    static RF_LRUTTL_EXP_RETURN_ERR: AtomicBool = AtomicBool::new(false);

    #[cached(
        key = "i32",
        convert = "{ x }",
        ttl_secs = 1,
        max_size = 16,
        result_fallback = true,
        force_refresh = "{ bypass }"
    )]
    fn rf_lru_ttl_expired_fn(x: i32, bypass: bool) -> Result<i32, String> {
        let _ = bypass;
        RF_LRUTTL_EXP_BODY_CALLS.fetch_add(1, Ordering::SeqCst);
        if RF_LRUTTL_EXP_RETURN_ERR.load(Ordering::SeqCst) {
            Err(format!("error for {x}"))
        } else {
            Ok(x * 10)
        }
    }

    #[test]
    fn lru_ttl_force_refresh_expired_entry_returns_stale_ok() {
        RF_LRUTTL_EXP_BODY_CALLS.store(0, Ordering::SeqCst);
        RF_LRUTTL_EXP_RETURN_ERR.store(false, Ordering::SeqCst);

        assert_eq!(rf_lru_ttl_expired_fn(300, false), Ok(3000), "seed call");
        assert_eq!(RF_LRUTTL_EXP_BODY_CALLS.load(Ordering::SeqCst), 1);

        std::thread::sleep(std::time::Duration::from_millis(1200));
        RF_LRUTTL_EXP_RETURN_ERR.store(true, Ordering::SeqCst);

        // Bypassed Err recompute over an EXPIRED LruTtlCache entry must recover the
        // stale Ok via the store's `cache_peek_with_expiry_status` override.
        assert_eq!(
            rf_lru_ttl_expired_fn(300, true),
            Ok(3000),
            "LruTtlCache: force-refresh Err over expired entry must return stale Ok"
        );
        assert_eq!(RF_LRUTTL_EXP_BODY_CALLS.load(Ordering::SeqCst), 2);
    }

    static RF_LRUTTL_LIVE_BODY_CALLS: AtomicUsize = AtomicUsize::new(0);
    static RF_LRUTTL_LIVE_RETURN_ERR: AtomicBool = AtomicBool::new(false);

    #[cached(
        key = "i32",
        convert = "{ x }",
        ttl_secs = 60,
        max_size = 16,
        result_fallback = true,
        force_refresh = "{ bypass }"
    )]
    fn rf_lru_ttl_live_fn(x: i32, bypass: bool) -> Result<i32, String> {
        let _ = bypass;
        RF_LRUTTL_LIVE_BODY_CALLS.fetch_add(1, Ordering::SeqCst);
        if RF_LRUTTL_LIVE_RETURN_ERR.load(Ordering::SeqCst) {
            Err(format!("error for {x}"))
        } else {
            Ok(x * 10)
        }
    }

    #[test]
    fn lru_ttl_force_refresh_live_entry_returns_stale_ok() {
        // Boundary companion: with a LIVE entry the fallback must still hold.
        RF_LRUTTL_LIVE_BODY_CALLS.store(0, Ordering::SeqCst);
        RF_LRUTTL_LIVE_RETURN_ERR.store(false, Ordering::SeqCst);

        assert_eq!(rf_lru_ttl_live_fn(301, false), Ok(3010), "seed call");
        assert_eq!(RF_LRUTTL_LIVE_BODY_CALLS.load(Ordering::SeqCst), 1);
        RF_LRUTTL_LIVE_RETURN_ERR.store(true, Ordering::SeqCst);
        assert_eq!(
            rf_lru_ttl_live_fn(301, true),
            Ok(3010),
            "LruTtlCache: force-refresh Err over live entry must return stale Ok"
        );
        assert_eq!(RF_LRUTTL_LIVE_BODY_CALLS.load(Ordering::SeqCst), 2);
    }
}

// ExpiringCache: `expires` (per-value expiry, deterministic, no sleeps).
#[cfg(feature = "time_stores")]
mod result_fallback_expiring_tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use cached::Expires;
    use cached::macros::cached;

    #[derive(Clone)]
    struct Val {
        n: i32,
        expired: bool,
    }
    impl Expires for Val {
        fn is_expired(&self) -> bool {
            self.expired
        }
    }

    static RF_EXP_E_BODY_CALLS: AtomicUsize = AtomicUsize::new(0);
    static RF_EXP_E_RETURN_ERR: AtomicBool = AtomicBool::new(false);

    #[cached(
        key = "i32",
        convert = "{ x }",
        expires = true,
        result_fallback = true,
        force_refresh = "{ bypass }"
    )]
    fn rf_expiring_expired_fn(x: i32, bypass: bool) -> Result<Val, String> {
        let _ = bypass;
        RF_EXP_E_BODY_CALLS.fetch_add(1, Ordering::SeqCst);
        if RF_EXP_E_RETURN_ERR.load(Ordering::SeqCst) {
            Err(format!("error for {x}"))
        } else {
            // Seed value is ALREADY expired-by-value, so the bypass peek must
            // surface it as (Some, true) rather than dropping it.
            Ok(Val {
                n: x * 10,
                expired: true,
            })
        }
    }

    #[test]
    fn expiring_force_refresh_expired_entry_returns_stale_ok() {
        RF_EXP_E_BODY_CALLS.store(0, Ordering::SeqCst);
        RF_EXP_E_RETURN_ERR.store(false, Ordering::SeqCst);

        let first = rf_expiring_expired_fn(400, false).expect("seed Ok");
        assert_eq!(first.n, 4000);
        assert_eq!(RF_EXP_E_BODY_CALLS.load(Ordering::SeqCst), 1);

        RF_EXP_E_RETURN_ERR.store(true, Ordering::SeqCst);

        // Bypassed Err over the per-value-expired ExpiringCache entry must recover
        // the stale Ok (pre-fix: cache_peek returned None for expired -> Err leaked).
        let second =
            rf_expiring_expired_fn(400, true).expect("expired-entry fallback must yield Ok");
        assert_eq!(
            second.n, 4000,
            "ExpiringCache: force-refresh Err over expired entry must return stale Ok"
        );
        assert_eq!(RF_EXP_E_BODY_CALLS.load(Ordering::SeqCst), 2);
    }

    static RF_EXP_L_BODY_CALLS: AtomicUsize = AtomicUsize::new(0);
    static RF_EXP_L_RETURN_ERR: AtomicBool = AtomicBool::new(false);

    #[cached(
        key = "i32",
        convert = "{ x }",
        expires = true,
        result_fallback = true,
        force_refresh = "{ bypass }"
    )]
    fn rf_expiring_live_fn(x: i32, bypass: bool) -> Result<Val, String> {
        let _ = bypass;
        RF_EXP_L_BODY_CALLS.fetch_add(1, Ordering::SeqCst);
        if RF_EXP_L_RETURN_ERR.load(Ordering::SeqCst) {
            Err(format!("error for {x}"))
        } else {
            Ok(Val {
                n: x * 10,
                expired: false,
            }) // live entry
        }
    }

    #[test]
    fn expiring_force_refresh_live_entry_returns_stale_ok() {
        RF_EXP_L_BODY_CALLS.store(0, Ordering::SeqCst);
        RF_EXP_L_RETURN_ERR.store(false, Ordering::SeqCst);

        let first = rf_expiring_live_fn(401, false).expect("seed Ok");
        assert_eq!(first.n, 4010);
        assert_eq!(RF_EXP_L_BODY_CALLS.load(Ordering::SeqCst), 1);

        RF_EXP_L_RETURN_ERR.store(true, Ordering::SeqCst);
        let second = rf_expiring_live_fn(401, true).expect("live-entry fallback must yield Ok");
        assert_eq!(
            second.n, 4010,
            "ExpiringCache: force-refresh Err over live entry must return stale Ok"
        );
        assert_eq!(RF_EXP_L_BODY_CALLS.load(Ordering::SeqCst), 2);
    }
}

// ExpiringLruCache: `expires` + `max_size`.
#[cfg(feature = "time_stores")]
mod result_fallback_expiring_lru_tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use cached::Expires;
    use cached::macros::cached;

    static RF_EXPLRU_BODY_CALLS: AtomicUsize = AtomicUsize::new(0);
    static RF_EXPLRU_RETURN_ERR: AtomicBool = AtomicBool::new(false);
    static RF_EXPLRU_MAKE_STALE: AtomicBool = AtomicBool::new(false);

    #[derive(Clone)]
    struct Val {
        n: i32,
        expired: bool,
    }
    impl Expires for Val {
        fn is_expired(&self) -> bool {
            self.expired
        }
    }

    #[cached(
        key = "i32",
        convert = "{ x }",
        expires = true,
        max_size = 16,
        result_fallback = true,
        force_refresh = "{ bypass }"
    )]
    fn rf_expiring_lru_fn(x: i32, bypass: bool) -> Result<Val, String> {
        let _ = bypass;
        RF_EXPLRU_BODY_CALLS.fetch_add(1, Ordering::SeqCst);
        if RF_EXPLRU_RETURN_ERR.load(Ordering::SeqCst) {
            Err(format!("error for {x}"))
        } else {
            Ok(Val {
                n: x * 10,
                expired: RF_EXPLRU_MAKE_STALE.load(Ordering::SeqCst),
            })
        }
    }

    #[test]
    fn expiring_lru_force_refresh_expired_entry_returns_stale_ok() {
        RF_EXPLRU_BODY_CALLS.store(0, Ordering::SeqCst);
        RF_EXPLRU_RETURN_ERR.store(false, Ordering::SeqCst);
        RF_EXPLRU_MAKE_STALE.store(true, Ordering::SeqCst);

        let first = rf_expiring_lru_fn(500, false).expect("seed Ok");
        assert_eq!(first.n, 5000);
        assert_eq!(RF_EXPLRU_BODY_CALLS.load(Ordering::SeqCst), 1);

        RF_EXPLRU_RETURN_ERR.store(true, Ordering::SeqCst);

        let second = rf_expiring_lru_fn(500, true).expect("expired-entry fallback must yield Ok");
        assert_eq!(
            second.n, 5000,
            "ExpiringLruCache: force-refresh Err over expired entry must return stale Ok"
        );
        assert_eq!(RF_EXPLRU_BODY_CALLS.load(Ordering::SeqCst), 2);
    }
}

// ── force_refresh predicate FALSE: normal early-return path is unaffected ──────
//
// Boundary: when the force_refresh predicate is false, the bypass arm is NOT taken,
// the renewing read serves the early-return on a fresh hit, and the body must not
// re-run. This pins that the rewired capture did not disturb the non-bypass branch.

#[cfg(feature = "time_stores")]
mod result_fallback_predicate_false_tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use cached::macros::cached;

    static RF_FALSE_BODY_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[cached(
        key = "i32",
        convert = "{ x }",
        ttl_secs = 60,
        result_fallback = true,
        force_refresh = "{ false }"
    )]
    fn rf_false_fn(x: i32) -> Result<i32, String> {
        RF_FALSE_BODY_CALLS.fetch_add(1, Ordering::SeqCst);
        Ok(x * 10)
    }

    #[test]
    fn predicate_false_serves_cache_without_rerunning_body() {
        RF_FALSE_BODY_CALLS.store(0, Ordering::SeqCst);

        assert_eq!(rf_false_fn(600), Ok(6000), "miss seeds the cache");
        assert_eq!(RF_FALSE_BODY_CALLS.load(Ordering::SeqCst), 1);

        // Fresh hit, predicate false -> early return, body must NOT re-run.
        assert_eq!(rf_false_fn(600), Ok(6000));
        assert_eq!(
            RF_FALSE_BODY_CALLS.load(Ordering::SeqCst),
            1,
            "predicate-false fresh hit must serve cache, not re-run body"
        );
    }
}

// ── async path: result_fallback + force_refresh expired-entry fallback ─────────
//
// The async `#[cached]` expansion routes the bypass capture through the same
// `cache_peek_with_expiry_status` call. This pins that an async bypassed Err over
// an EXPIRED TtlCache entry still recovers the stale Ok fallback.

#[cfg(all(feature = "async", feature = "time_stores"))]
mod result_fallback_async_tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use cached::macros::cached;

    static RF_ASYNC_BODY_CALLS: AtomicUsize = AtomicUsize::new(0);
    static RF_ASYNC_RETURN_ERR: AtomicBool = AtomicBool::new(false);

    #[cached(
        key = "i32",
        convert = "{ x }",
        ttl_secs = 1,
        result_fallback = true,
        force_refresh = "{ bypass }"
    )]
    async fn rf_async_fn(x: i32, bypass: bool) -> Result<i32, String> {
        let _ = bypass;
        RF_ASYNC_BODY_CALLS.fetch_add(1, Ordering::SeqCst);
        if RF_ASYNC_RETURN_ERR.load(Ordering::SeqCst) {
            Err(format!("error for {x}"))
        } else {
            Ok(x * 10)
        }
    }

    #[tokio::test]
    async fn async_force_refresh_expired_entry_returns_stale_ok() {
        RF_ASYNC_BODY_CALLS.store(0, Ordering::SeqCst);
        RF_ASYNC_RETURN_ERR.store(false, Ordering::SeqCst);

        assert_eq!(rf_async_fn(700, false).await, Ok(7000), "seed call");
        assert_eq!(RF_ASYNC_BODY_CALLS.load(Ordering::SeqCst), 1);

        tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
        RF_ASYNC_RETURN_ERR.store(true, Ordering::SeqCst);

        assert_eq!(
            rf_async_fn(700, true).await,
            Ok(7000),
            "async: force-refresh Err over expired entry must return stale Ok"
        );
        assert_eq!(RF_ASYNC_BODY_CALLS.load(Ordering::SeqCst), 2);
    }
}
