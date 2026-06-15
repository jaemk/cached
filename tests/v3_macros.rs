/*!
Integration tests for the 3.0 macro changes:

- (#230/#114): macro-introduced bindings no longer collide with user args
  named `key`/`cache`/`result` (the confirmed repro) under all three macros.
- (#202/#203): reference inputs (`&str`/`Option<&str>`/`&String`) form an
  owned default key without a `convert` block.
- (#149): the new `ttl_millis` attribute (recompute after a sub-second TTL),
  gated on `time_stores`.
- (#146): the new `force_refresh` attribute (bypass the cache on demand).
- (#16/#140): `in_impl = true` caches a method that takes `self`.
*/

#![cfg(feature = "proc_macro")]
// Several tests intentionally take `&String` / other ref args to exercise the
// macro's default-key handling for reference inputs (#202/#203).
#![allow(clippy::ptr_arg)]

use cached::macros::{cached, concurrent_cached, once};

// ── (#230/#114): user args named like macro internals ──────────────────────
// Before the binding-hygiene fix, a function argument named `key`, `cache`, or
// `result` shadowed the macro-introduced locals and failed to compile.

static COLLIDE_CACHED_CALLS: AtomicUsize = AtomicUsize::new(0);
static COLLIDE_ONCE_CALLS: AtomicUsize = AtomicUsize::new(0);
static COLLIDE_CONCURRENT_CALLS: AtomicUsize = AtomicUsize::new(0);

#[cached]
fn collide_cached(key: i32, cache: i32, result: i32) -> i32 {
    COLLIDE_CACHED_CALLS.fetch_add(1, Ordering::SeqCst);
    key + cache + result
}

#[once]
fn collide_once(key: i32, cache: i32, result: i32) -> i32 {
    COLLIDE_ONCE_CALLS.fetch_add(1, Ordering::SeqCst);
    key + cache + result
}

#[concurrent_cached]
fn collide_concurrent(key: i32, cache: i32, result: i32) -> i32 {
    COLLIDE_CONCURRENT_CALLS.fetch_add(1, Ordering::SeqCst);
    key + cache + result
}

#[test]
fn arg_name_collisions_compile_and_cache() {
    // Reset counters so the test does not depend on execution order.
    COLLIDE_CACHED_CALLS.store(0, Ordering::SeqCst);
    COLLIDE_ONCE_CALLS.store(0, Ordering::SeqCst);
    COLLIDE_CONCURRENT_CALLS.store(0, Ordering::SeqCst);

    // #[cached]: two calls with the same args hit the cache; body runs once.
    assert_eq!(collide_cached(1, 2, 3), 6);
    assert_eq!(collide_cached(1, 2, 3), 6); // cached hit, same key
    assert_eq!(
        COLLIDE_CACHED_CALLS.load(Ordering::SeqCst),
        1,
        "#[cached]: second same-arg call must be a cache hit (body runs once)"
    );
    assert_eq!(collide_cached(10, 20, 30), 60);

    // `#[once]` caches the first produced value for all later calls.
    assert_eq!(collide_once(1, 2, 3), 6);
    assert_eq!(collide_once(4, 5, 6), 6); // once: single value, second call is a cache hit
    assert_eq!(
        COLLIDE_ONCE_CALLS.load(Ordering::SeqCst),
        1,
        "#[once]: second call with different args must be a cache hit (body runs once)"
    );

    // #[concurrent_cached]: two calls with the same args hit the cache; body runs once.
    assert_eq!(collide_concurrent(1, 2, 3), 6);
    assert_eq!(collide_concurrent(1, 2, 3), 6);
    assert_eq!(
        COLLIDE_CONCURRENT_CALLS.load(Ordering::SeqCst),
        1,
        "#[concurrent_cached]: second same-arg call must be a cache hit (body runs once)"
    );
    assert_eq!(collide_concurrent(7, 8, 9), 24);
}

// ── (#202/#203): reference inputs form an owned default key ────────────────
// `&str`, `Option<&str>`, and `&String` should produce an owned key (`String` /
// `Option<String>`) without an explicit `key`/`convert`.

use std::sync::atomic::{AtomicUsize, Ordering};

static STR_CALLS: AtomicUsize = AtomicUsize::new(0);
static OPT_CALLS: AtomicUsize = AtomicUsize::new(0);
static STRING_CALLS: AtomicUsize = AtomicUsize::new(0);

#[cached]
fn ref_str_len(s: &str) -> usize {
    STR_CALLS.fetch_add(1, Ordering::SeqCst);
    s.len()
}

#[cached]
fn opt_ref_str_len(o: Option<&str>) -> usize {
    OPT_CALLS.fetch_add(1, Ordering::SeqCst);
    o.map_or(0, |s| s.len())
}

// `&String` is intentional here: this exercises the macro's `&T` default-key
// handling (#202/#203), not idiomatic API design (see the file-level
// `allow(clippy::ptr_arg)`).
#[cached]
fn ref_string_len(s: &String) -> usize {
    STRING_CALLS.fetch_add(1, Ordering::SeqCst);
    s.len()
}

// Note: `STR_CALLS`/`OPT_CALLS`/`STRING_CALLS` and their cache statics are owned
// exclusively by this test. The counters are reset below; the underlying caches
// cannot be reset from here (function-local or module-static), so the assertions
// remain valid only on first call per entry (which holds since this test is the
// sole caller of these functions).
#[test]
fn reference_inputs_default_key() {
    // Reset counters so the assertions are independent of execution order.
    STR_CALLS.store(0, Ordering::SeqCst);
    OPT_CALLS.store(0, Ordering::SeqCst);
    STRING_CALLS.store(0, Ordering::SeqCst);

    assert_eq!(ref_str_len("hello"), 5);
    assert_eq!(ref_str_len("hello"), 5);
    assert_eq!(
        STR_CALLS.load(Ordering::SeqCst),
        1,
        "second call should hit cache"
    );
    assert_eq!(ref_str_len("hi"), 2);
    assert_eq!(STR_CALLS.load(Ordering::SeqCst), 2);

    assert_eq!(opt_ref_str_len(Some("hello")), 5);
    assert_eq!(opt_ref_str_len(Some("hello")), 5);
    assert_eq!(OPT_CALLS.load(Ordering::SeqCst), 1);
    assert_eq!(opt_ref_str_len(None), 0);
    assert_eq!(opt_ref_str_len(None), 0);
    assert_eq!(OPT_CALLS.load(Ordering::SeqCst), 2);

    let owned = String::from("world!");
    assert_eq!(ref_string_len(&owned), 6);
    assert_eq!(ref_string_len(&owned), 6);
    assert_eq!(STRING_CALLS.load(Ordering::SeqCst), 1);
}

// ── (#146): force_refresh bypasses the cache on demand ─────────────────────

static FORCE_CALLS: AtomicUsize = AtomicUsize::new(0);
// The returned value tracks an externally controllable source so the test can
// distinguish "served the stale cached value" from "recomputed + overwrote".
static FORCE_SOURCE: AtomicUsize = AtomicUsize::new(1);

// `bypass` is excluded from the cache key via `key`/`convert` so the same entry
// is hit/refreshed regardless of the flag.
#[cached(key = "i32", convert = "{ x }", force_refresh = "{ bypass }")]
fn force_refresh_fn(x: i32, bypass: bool) -> usize {
    let _ = bypass; // used by the generated force_refresh guard, not the body
    FORCE_CALLS.fetch_add(1, Ordering::SeqCst);
    x as usize + FORCE_SOURCE.load(Ordering::SeqCst)
}

#[test]
fn force_refresh_bypasses_cache() {
    // `FORCE_CALLS`/`force_refresh_fn` are exclusive to this test, but reset the
    // counter so the absolute assertions do not depend on its initial value.
    FORCE_CALLS.store(0, Ordering::SeqCst);
    FORCE_SOURCE.store(1, Ordering::SeqCst);
    let first = force_refresh_fn(1, false); // miss → 1 + 1 = 2
    assert_eq!(first, 2);
    assert_eq!(FORCE_CALLS.load(Ordering::SeqCst), 1);
    // bypass=false: cached hit, no recompute even though the source changed.
    FORCE_SOURCE.store(100, Ordering::SeqCst);
    let hit = force_refresh_fn(1, false);
    assert_eq!(hit, 2, "served the stale cached value");
    assert_eq!(FORCE_CALLS.load(Ordering::SeqCst), 1);
    // bypass=true: recompute + overwrite even though the key is cached.
    let refreshed = force_refresh_fn(1, true);
    assert_eq!(refreshed, 101, "recomputed against the new source");
    assert_eq!(FORCE_CALLS.load(Ordering::SeqCst), 2);
    // After the overwrite, a non-bypass call serves the refreshed value.
    let after = force_refresh_fn(1, false);
    assert_eq!(after, 101, "force_refresh overwrote the cache entry");
    assert_eq!(FORCE_CALLS.load(Ordering::SeqCst), 2);
}

static FORCE_CONC_CALLS: AtomicUsize = AtomicUsize::new(0);
static FORCE_CONC_SOURCE: AtomicUsize = AtomicUsize::new(1);

#[concurrent_cached(key = "i32", convert = "{ x }", force_refresh = "{ bypass }")]
fn force_refresh_concurrent(x: i32, bypass: bool) -> usize {
    let _ = bypass; // used by the generated force_refresh guard, not the body
    FORCE_CONC_CALLS.fetch_add(1, Ordering::SeqCst);
    x as usize + FORCE_CONC_SOURCE.load(Ordering::SeqCst)
}

#[test]
fn force_refresh_concurrent_bypasses_cache() {
    // Exclusive statics; reset the counter so the absolute assertions do not
    // depend on its initial value.
    FORCE_CONC_CALLS.store(0, Ordering::SeqCst);
    FORCE_CONC_SOURCE.store(1, Ordering::SeqCst);
    let first = force_refresh_concurrent(2, false); // 2 + 1 = 3
    assert_eq!(first, 3);
    assert_eq!(FORCE_CONC_CALLS.load(Ordering::SeqCst), 1);
    FORCE_CONC_SOURCE.store(100, Ordering::SeqCst);
    let hit = force_refresh_concurrent(2, false);
    assert_eq!(hit, 3, "served the stale cached value");
    assert_eq!(FORCE_CONC_CALLS.load(Ordering::SeqCst), 1);
    let refreshed = force_refresh_concurrent(2, true);
    assert_eq!(refreshed, 102, "recomputed against the new source");
    assert_eq!(FORCE_CONC_CALLS.load(Ordering::SeqCst), 2);
    let after = force_refresh_concurrent(2, false);
    assert_eq!(after, 102, "force_refresh overwrote the cache entry");
    assert_eq!(FORCE_CONC_CALLS.load(Ordering::SeqCst), 2);
}

// force_refresh as an arbitrary expression over an existing argument (no dedicated
// flag, default key): recompute whenever the predicate over the args holds. This is
// the canonical form: the block is evaluated, it does not introduce a bool param.
static FORCE_EXPR_CALLS: AtomicUsize = AtomicUsize::new(0);

#[cached(force_refresh = "{ x == 0 }")]
fn force_refresh_expr(x: i32) -> usize {
    FORCE_EXPR_CALLS.fetch_add(1, Ordering::SeqCst);
    x as usize
}

#[test]
fn force_refresh_expression_over_args() {
    FORCE_EXPR_CALLS.store(0, Ordering::SeqCst);
    // x == 0: predicate true, so every call bypasses the cache and recomputes.
    let _ = force_refresh_expr(0);
    let _ = force_refresh_expr(0);
    assert_eq!(
        FORCE_EXPR_CALLS.load(Ordering::SeqCst),
        2,
        "x==0 bypasses every call"
    );
    // x != 0: predicate false, normal caching (one compute, then hits).
    let _ = force_refresh_expr(5);
    let _ = force_refresh_expr(5);
    assert_eq!(
        FORCE_EXPR_CALLS.load(Ordering::SeqCst),
        3,
        "x!=0 served from cache"
    );
}

// Documents WHY a dedicated flag must be excluded from the key (see the
// `force_refresh` attribute docs). With the DEFAULT key the flag is part of the
// key, so a `refresh = true` call recomputes into the `(x, true)` entry while
// ordinary `refresh = false` calls read the `(x, false)` entry and never see the
// refreshed value. The `force_refresh_*` tests above use `key`/`convert` to
// exclude the flag, which is the correct pattern.
static FOOTGUN_CALLS: AtomicUsize = AtomicUsize::new(0);
static FOOTGUN_SOURCE: AtomicUsize = AtomicUsize::new(1);

#[cached(force_refresh = "{ refresh }")]
fn force_refresh_default_key(x: i32, refresh: bool) -> usize {
    let _ = refresh;
    FOOTGUN_CALLS.fetch_add(1, Ordering::SeqCst);
    x as usize + FOOTGUN_SOURCE.load(Ordering::SeqCst)
}

#[test]
fn force_refresh_default_key_does_not_update_normal_slot() {
    FOOTGUN_CALLS.store(0, Ordering::SeqCst);
    FOOTGUN_SOURCE.store(1, Ordering::SeqCst);
    assert_eq!(force_refresh_default_key(1, false), 2); // miss: stores (1,false)=2, body runs (count=1)
    FOOTGUN_SOURCE.store(100, Ordering::SeqCst);
    // refresh=true recomputes the fresh value (101) but stores it under (1,true), body runs (count=2).
    assert_eq!(force_refresh_default_key(1, true), 101);
    // A normal refresh=false call still reads the stale (1,false) entry; body does NOT run (count stays 2).
    assert_eq!(
        force_refresh_default_key(1, false),
        2,
        "default key: forced refresh writes a separate (x,true) slot, not seen here"
    );
    assert_eq!(
        FOOTGUN_CALLS.load(Ordering::SeqCst),
        2,
        "body ran exactly twice: once for the initial miss and once for the force-refresh"
    );
}

// ── force_refresh on #[once] ───────────────────────────────────────────────
// `#[once]` stores one value for all callers. `force_refresh` bypasses that single
// value and recomputes/overwrites it. Unlike `#[cached]` there is no key, so there
// is no "(x, true)" slot footgun: the refreshed value is what every later call sees.

static ONCE_FR_CALLS: AtomicUsize = AtomicUsize::new(0);
// Separate source so the cached return value is independent of the call counter;
// this lets us reset ONCE_FR_CALLS without coupling the counter to the cached value.
static ONCE_FR_SOURCE: AtomicUsize = AtomicUsize::new(10);

// NOTE: this fn must remain call-exclusive to `once_force_refresh_recomputes_shared_value`.
#[once(force_refresh = "{ bypass }")]
fn once_force_refresh(bypass: bool) -> usize {
    let _ = bypass; // used by the generated force_refresh guard, not the body
    ONCE_FR_CALLS.fetch_add(1, Ordering::SeqCst);
    ONCE_FR_SOURCE.load(Ordering::SeqCst)
}

// Note: `ONCE_FR_CALLS` is reset below; the underlying `#[once]` cache static
// cannot be reset from here (function-local), so the test is the sole caller of
// `once_force_refresh`.
#[test]
fn once_force_refresh_recomputes_shared_value() {
    ONCE_FR_CALLS.store(0, Ordering::SeqCst);
    ONCE_FR_SOURCE.store(10, Ordering::SeqCst);

    // First call computes and caches the single shared value.
    let first = once_force_refresh(false);
    assert_eq!(first, 10);
    assert_eq!(ONCE_FR_CALLS.load(Ordering::SeqCst), 1);

    // Non-bypass hit: cached value returned, body not re-run.
    ONCE_FR_SOURCE.store(99, Ordering::SeqCst); // would change value if body ran
    let hit = once_force_refresh(false);
    assert_eq!(hit, first, "cached hit, body not re-run");
    assert_eq!(ONCE_FR_CALLS.load(Ordering::SeqCst), 1);

    // Bypass: recompute and overwrite the single shared value.
    let refreshed = once_force_refresh(true);
    assert_eq!(ONCE_FR_CALLS.load(Ordering::SeqCst), 2);
    assert_eq!(
        refreshed, 99,
        "force_refresh recomputed against the new source"
    );
    assert_ne!(refreshed, first, "force_refresh produced a new value");

    // Subsequent non-bypass call serves the refreshed (overwritten) value — no
    // separate keyed slot, unlike the `#[cached]` default-key footgun above.
    let after = once_force_refresh(false);
    assert_eq!(after, refreshed, "later calls see the overwritten value");
    assert_eq!(ONCE_FR_CALLS.load(Ordering::SeqCst), 2);
}

// ── force_refresh + result_fallback compose ───────────────────────────────
// `result_fallback` keeps the prior `Ok` and serves it when a refresh returns
// `Err`; `force_refresh` decides when to bypass the cached value and re-run the
// body. Together: an `Err` recompute falls back to the last `Ok`, an `Ok`
// recompute overwrites. Requires a `CloneCached` store, so this uses `ttl`
// (gated on `time_stores`).

#[cfg(feature = "time_stores")]
mod force_refresh_result_fallback {
    use super::*;

    static FB_CALLS: AtomicUsize = AtomicUsize::new(0);
    // 0 => return Err; non-zero => return Ok(value).
    static FB_SOURCE: AtomicUsize = AtomicUsize::new(0);

    // `bypass` is excluded from the key so the same entry is hit/bypassed.
    // A long `ttl` keeps entries fresh; bypass (not expiry) drives recompute.
    #[cached(
        key = "i32",
        convert = "{ x }",
        ttl_secs = 600,
        result_fallback = true,
        force_refresh = "{ bypass }"
    )]
    fn fb_fn(x: i32, bypass: bool) -> Result<usize, ()> {
        let _ = bypass; // used by the generated force_refresh guard, not the body
        FB_CALLS.fetch_add(1, Ordering::SeqCst);
        match FB_SOURCE.load(Ordering::SeqCst) {
            0 => Err(()),
            v => Ok(x as usize + v),
        }
    }

    #[test]
    fn err_falls_back_force_refresh_recomputes_on_ok() {
        FB_CALLS.store(0, Ordering::SeqCst);
        // First call: Ok(10), cached.
        FB_SOURCE.store(10, Ordering::SeqCst);
        assert_eq!(fb_fn(1, false), Ok(11));
        assert_eq!(FB_CALLS.load(Ordering::SeqCst), 1);

        // Non-bypass hit: served from cache, body not re-run.
        FB_SOURCE.store(0, Ordering::SeqCst); // would Err if run
        assert_eq!(fb_fn(1, false), Ok(11), "cached hit, body not re-run");
        assert_eq!(FB_CALLS.load(Ordering::SeqCst), 1);

        // Bypass with the source returning Err: body runs, falls back to last Ok.
        assert_eq!(fb_fn(1, true), Ok(11), "Err refresh falls back to last Ok");
        assert_eq!(FB_CALLS.load(Ordering::SeqCst), 2);

        // Bypass with the source returning Ok: body runs and overwrites.
        FB_SOURCE.store(50, Ordering::SeqCst);
        assert_eq!(fb_fn(1, true), Ok(51), "Ok refresh recomputes + overwrites");
        assert_eq!(FB_CALLS.load(Ordering::SeqCst), 3);

        // Non-bypass call now serves the refreshed value.
        FB_SOURCE.store(0, Ordering::SeqCst);
        assert_eq!(fb_fn(1, false), Ok(51), "serves the overwritten value");
        assert_eq!(FB_CALLS.load(Ordering::SeqCst), 3);
    }

    // (#146 / FIX 3b): a force_refresh bypass on the `result_fallback` path must not
    // have read side effects on the bypassed entry. `result_fallback` captures the
    // prior `Ok` via the renewing `cache_get_with_expiry_status` only for the genuine
    // early-return; on a bypass it uses the non-renewing `CachedPeek::cache_peek`.
    // The deterministic signal: a live hit through the renewing read increments the
    // store's hits counter, while `cache_peek` does not. So after a force_refresh
    // bypass the hits counter must stay 0. (Pre-fix it incremented on every bypass.)
    static FRSE_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[cached(
        name = "FRSE_CACHE",
        key = "i32",
        convert = "{ x }",
        ttl_secs = 600,
        result_fallback = true,
        force_refresh = "{ bypass }"
    )]
    fn frse_fn(x: i32, bypass: bool) -> Result<usize, ()> {
        let _ = bypass; // consumed by the generated force_refresh guard, not the body
        FRSE_CALLS.fetch_add(1, Ordering::SeqCst);
        Ok(x as usize + 1)
    }

    #[test]
    fn force_refresh_bypass_has_no_read_side_effects() {
        use cached::Cached;

        FRSE_CALLS.store(0, Ordering::SeqCst);
        // Seed the entry (a miss, then it is set). No hit yet.
        assert_eq!(frse_fn(1, false), Ok(2));
        assert_eq!(FRSE_CALLS.load(Ordering::SeqCst), 1);
        assert_eq!(
            FRSE_CACHE.read().cache_hits(),
            Some(0),
            "seeding the entry is a miss + set, not a hit"
        );

        // Bypass the cached entry several times. Each bypass recomputes the body.
        // The bypassed entry must NOT be read through the renewing path, so the
        // hits counter must remain 0.
        assert_eq!(frse_fn(1, true), Ok(2));
        assert_eq!(frse_fn(1, true), Ok(2));
        assert_eq!(frse_fn(1, true), Ok(2));
        assert_eq!(
            FRSE_CALLS.load(Ordering::SeqCst),
            4,
            "each bypass recomputes"
        );
        assert_eq!(
            FRSE_CACHE.read().cache_hits(),
            Some(0),
            "force_refresh bypass must not hit-count the bypassed entry (#146)"
        );

        // A genuine (non-bypass) hit still counts: this confirms the renewing read
        // path is intact for the early-return case.
        assert_eq!(frse_fn(1, false), Ok(2));
        assert_eq!(
            FRSE_CALLS.load(Ordering::SeqCst),
            4,
            "non-bypass served from cache"
        );
        assert_eq!(
            FRSE_CACHE.read().cache_hits(),
            Some(1),
            "a real early-return hit increments the counter"
        );
    }

    static CFB_CALLS: AtomicUsize = AtomicUsize::new(0);
    static CFB_SOURCE: AtomicUsize = AtomicUsize::new(0);

    // `#[concurrent_cached]` folds the `result_fallback` lookup into a different
    // code path than `#[cached]` (via `ConcurrentCloneCached` inside the set
    // block), so the `result_fallback` + `force_refresh` composition needs its
    // own coverage on this macro.
    #[concurrent_cached(
        key = "i32",
        convert = "{ x }",
        ttl_secs = 600,
        result_fallback = true,
        force_refresh = "{ bypass }"
    )]
    fn cfb_fn(x: i32, bypass: bool) -> Result<usize, ()> {
        let _ = bypass; // used by the generated force_refresh guard, not the body
        CFB_CALLS.fetch_add(1, Ordering::SeqCst);
        match CFB_SOURCE.load(Ordering::SeqCst) {
            0 => Err(()),
            v => Ok(x as usize + v),
        }
    }

    #[test]
    fn concurrent_err_falls_back_force_refresh_recomputes_on_ok() {
        CFB_CALLS.store(0, Ordering::SeqCst);
        // First call: Ok(10), cached.
        CFB_SOURCE.store(10, Ordering::SeqCst);
        assert_eq!(cfb_fn(1, false), Ok(11));
        assert_eq!(CFB_CALLS.load(Ordering::SeqCst), 1);

        // Non-bypass hit: served from cache, body not re-run.
        CFB_SOURCE.store(0, Ordering::SeqCst); // would Err if run
        assert_eq!(cfb_fn(1, false), Ok(11), "cached hit, body not re-run");
        assert_eq!(CFB_CALLS.load(Ordering::SeqCst), 1);

        // Bypass with the source returning Err: body runs, falls back to last Ok.
        assert_eq!(cfb_fn(1, true), Ok(11), "Err refresh falls back to last Ok");
        assert_eq!(CFB_CALLS.load(Ordering::SeqCst), 2);

        // Bypass with the source returning Ok: body runs and overwrites.
        CFB_SOURCE.store(50, Ordering::SeqCst);
        assert_eq!(
            cfb_fn(1, true),
            Ok(51),
            "Ok refresh recomputes + overwrites"
        );
        assert_eq!(CFB_CALLS.load(Ordering::SeqCst), 3);

        // Non-bypass call now serves the refreshed value.
        CFB_SOURCE.store(0, Ordering::SeqCst);
        assert_eq!(cfb_fn(1, false), Ok(51), "serves the overwritten value");
        assert_eq!(CFB_CALLS.load(Ordering::SeqCst), 3);
    }

    // (#146): the `#[concurrent_cached]` analogue of
    // `force_refresh_bypass_has_no_read_side_effects`. A force_refresh bypass on the
    // `result_fallback` path must not read the bypassed entry through the renewing
    // `cache_get_with_expiry_status` (which would increment the sharded store's hits
    // counter); it must use the non-renewing `cache_peek_with_expiry_status` instead.
    // The deterministic signal is the underlying `ShardedTtlCache`'s hits metric:
    // it must stay 0 across bypass calls and increment only on a genuine non-bypass hit.
    // (Pre-fix: pointing the bypass read back at the renewing `cache_get_with_expiry_status`
    // makes the hits counter climb on every bypass, so this test fails.)
    static CFRSE_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[concurrent_cached(
        name = "CFRSE_CACHE",
        key = "i32",
        convert = "{ x }",
        ttl_secs = 600,
        result_fallback = true,
        force_refresh = "{ bypass }"
    )]
    fn cfrse_fn(x: i32, bypass: bool) -> Result<usize, ()> {
        let _ = bypass; // consumed by the generated force_refresh guard, not the body
        CFRSE_CALLS.fetch_add(1, Ordering::SeqCst);
        Ok(x as usize + 1)
    }

    #[test]
    fn concurrent_force_refresh_bypass_has_no_read_side_effects() {
        CFRSE_CALLS.store(0, Ordering::SeqCst);
        // Seed the entry (a miss, then it is set). No hit yet.
        assert_eq!(cfrse_fn(1, false), Ok(2));
        assert_eq!(CFRSE_CALLS.load(Ordering::SeqCst), 1);
        assert_eq!(
            CFRSE_CACHE.metrics().hits,
            Some(0),
            "seeding the entry is a miss + set, not a hit"
        );

        // Bypass the cached entry several times. Each bypass recomputes the body and
        // must read the stale fallback via the non-renewing peek, leaving hits at 0.
        assert_eq!(cfrse_fn(1, true), Ok(2));
        assert_eq!(cfrse_fn(1, true), Ok(2));
        assert_eq!(cfrse_fn(1, true), Ok(2));
        assert_eq!(
            CFRSE_CALLS.load(Ordering::SeqCst),
            4,
            "each bypass recomputes"
        );
        assert_eq!(
            CFRSE_CACHE.metrics().hits,
            Some(0),
            "force_refresh bypass must not hit-count the bypassed entry (#146)"
        );

        // A genuine (non-bypass) hit still counts: this confirms the renewing read
        // path is intact for the early-return case.
        assert_eq!(cfrse_fn(1, false), Ok(2));
        assert_eq!(
            CFRSE_CALLS.load(Ordering::SeqCst),
            4,
            "non-bypass served from cache"
        );
        assert_eq!(
            CFRSE_CACHE.metrics().hits,
            Some(1),
            "a real early-return hit increments the counter"
        );
    }

    // ── result_fallback + force_refresh on an in_impl method ──────────────────
    // Combines the function-local-static in_impl path with `result_fallback`: an
    // `Err` refresh falls back to the last `Ok`, an `Ok` refresh overwrites. This
    // mirrors `err_falls_back_force_refresh_recomputes_on_ok` but on a `self`-method.

    struct ImplFallback;

    static IMPL_FB_CALLS: AtomicUsize = AtomicUsize::new(0);
    static IMPL_FB_SOURCE: AtomicUsize = AtomicUsize::new(0);

    impl ImplFallback {
        #[cached(
            in_impl = true,
            key = "i32",
            convert = "{ x }",
            ttl_secs = 600,
            result_fallback = true,
            force_refresh = "{ bypass }"
        )]
        fn fb_method(&self, x: i32, bypass: bool) -> Result<usize, ()> {
            let _ = bypass; // consumed by the generated force_refresh guard
            IMPL_FB_CALLS.fetch_add(1, Ordering::SeqCst);
            match IMPL_FB_SOURCE.load(Ordering::SeqCst) {
                0 => Err(()),
                v => Ok(x as usize + v),
            }
        }
    }

    #[test]
    fn in_impl_err_falls_back_force_refresh_recomputes_on_ok() {
        IMPL_FB_CALLS.store(0, Ordering::SeqCst);
        let s = ImplFallback;
        // First call: Ok(10), cached.
        IMPL_FB_SOURCE.store(10, Ordering::SeqCst);
        assert_eq!(s.fb_method(1, false), Ok(11));
        assert_eq!(IMPL_FB_CALLS.load(Ordering::SeqCst), 1);

        // Non-bypass hit: served from cache, body not re-run.
        IMPL_FB_SOURCE.store(0, Ordering::SeqCst); // would Err if run
        assert_eq!(s.fb_method(1, false), Ok(11), "cached hit, body not re-run");
        assert_eq!(IMPL_FB_CALLS.load(Ordering::SeqCst), 1);

        // Bypass with the source returning Err: body runs, falls back to last Ok.
        assert_eq!(
            s.fb_method(1, true),
            Ok(11),
            "Err refresh falls back to last Ok"
        );
        assert_eq!(IMPL_FB_CALLS.load(Ordering::SeqCst), 2);

        // Bypass with the source returning Ok: body runs and overwrites.
        IMPL_FB_SOURCE.store(50, Ordering::SeqCst);
        assert_eq!(
            s.fb_method(1, true),
            Ok(51),
            "Ok refresh recomputes + overwrites"
        );
        assert_eq!(IMPL_FB_CALLS.load(Ordering::SeqCst), 3);

        // Non-bypass call now serves the refreshed value.
        IMPL_FB_SOURCE.store(0, Ordering::SeqCst);
        assert_eq!(
            s.fb_method(1, false),
            Ok(51),
            "serves the overwritten value"
        );
        assert_eq!(IMPL_FB_CALLS.load(Ordering::SeqCst), 3);
    }

    // ── result_fallback + force_refresh with the ttl_millis duration form ─────
    // Same composition as the `ttl_secs = 600` tests above, but the TTL is expressed via
    // `ttl_millis`. This proves the millis duration threads into the TTL store on the
    // fallback path: if the duration were dropped the store would have no TTL and
    // `result_fallback` (which needs a CloneCached TTL store) would be a no-op.

    static FB_MILLIS_CALLS: AtomicUsize = AtomicUsize::new(0);
    static FB_MILLIS_SOURCE: AtomicUsize = AtomicUsize::new(0);

    #[cached(
        key = "i32",
        convert = "{ x }",
        ttl_millis = 600_000,
        result_fallback = true,
        force_refresh = "{ bypass }"
    )]
    fn fb_millis_fn(x: i32, bypass: bool) -> Result<usize, ()> {
        let _ = bypass; // used by the generated force_refresh guard, not the body
        FB_MILLIS_CALLS.fetch_add(1, Ordering::SeqCst);
        match FB_MILLIS_SOURCE.load(Ordering::SeqCst) {
            0 => Err(()),
            v => Ok(x as usize + v),
        }
    }

    #[test]
    fn err_falls_back_force_refresh_recomputes_on_ok_ttl_millis() {
        FB_MILLIS_CALLS.store(0, Ordering::SeqCst);
        // First call: Ok(10), cached.
        FB_MILLIS_SOURCE.store(10, Ordering::SeqCst);
        assert_eq!(fb_millis_fn(1, false), Ok(11));
        assert_eq!(FB_MILLIS_CALLS.load(Ordering::SeqCst), 1);

        // Non-bypass hit: served from cache, body not re-run.
        FB_MILLIS_SOURCE.store(0, Ordering::SeqCst); // would Err if run
        assert_eq!(
            fb_millis_fn(1, false),
            Ok(11),
            "cached hit, body not re-run"
        );
        assert_eq!(FB_MILLIS_CALLS.load(Ordering::SeqCst), 1);

        // Bypass with the source returning Err: body runs, falls back to last Ok.
        assert_eq!(
            fb_millis_fn(1, true),
            Ok(11),
            "Err refresh falls back to last Ok"
        );
        assert_eq!(FB_MILLIS_CALLS.load(Ordering::SeqCst), 2);

        // Bypass with the source returning Ok: body runs and overwrites.
        FB_MILLIS_SOURCE.store(50, Ordering::SeqCst);
        assert_eq!(
            fb_millis_fn(1, true),
            Ok(51),
            "Ok refresh recomputes + overwrites"
        );
        assert_eq!(FB_MILLIS_CALLS.load(Ordering::SeqCst), 3);

        // Non-bypass call now serves the refreshed value.
        FB_MILLIS_SOURCE.store(0, Ordering::SeqCst);
        assert_eq!(
            fb_millis_fn(1, false),
            Ok(51),
            "serves the overwritten value"
        );
        assert_eq!(FB_MILLIS_CALLS.load(Ordering::SeqCst), 3);
    }

    static CFB_MILLIS_CALLS: AtomicUsize = AtomicUsize::new(0);
    static CFB_MILLIS_SOURCE: AtomicUsize = AtomicUsize::new(0);

    // The `#[concurrent_cached]` analogue: confirms the millis duration also threads
    // into the sharded TTL store on the concurrent fallback path.
    #[concurrent_cached(
        key = "i32",
        convert = "{ x }",
        ttl_millis = 600_000,
        result_fallback = true,
        force_refresh = "{ bypass }"
    )]
    fn cfb_millis_fn(x: i32, bypass: bool) -> Result<usize, ()> {
        let _ = bypass; // used by the generated force_refresh guard, not the body
        CFB_MILLIS_CALLS.fetch_add(1, Ordering::SeqCst);
        match CFB_MILLIS_SOURCE.load(Ordering::SeqCst) {
            0 => Err(()),
            v => Ok(x as usize + v),
        }
    }

    #[test]
    fn concurrent_err_falls_back_force_refresh_recomputes_on_ok_ttl_millis() {
        CFB_MILLIS_CALLS.store(0, Ordering::SeqCst);
        // First call: Ok(10), cached.
        CFB_MILLIS_SOURCE.store(10, Ordering::SeqCst);
        assert_eq!(cfb_millis_fn(1, false), Ok(11));
        assert_eq!(CFB_MILLIS_CALLS.load(Ordering::SeqCst), 1);

        // Non-bypass hit: served from cache, body not re-run.
        CFB_MILLIS_SOURCE.store(0, Ordering::SeqCst); // would Err if run
        assert_eq!(
            cfb_millis_fn(1, false),
            Ok(11),
            "cached hit, body not re-run"
        );
        assert_eq!(CFB_MILLIS_CALLS.load(Ordering::SeqCst), 1);

        // Bypass with the source returning Err: body runs, falls back to last Ok.
        assert_eq!(
            cfb_millis_fn(1, true),
            Ok(11),
            "Err refresh falls back to last Ok"
        );
        assert_eq!(CFB_MILLIS_CALLS.load(Ordering::SeqCst), 2);

        // Bypass with the source returning Ok: body runs and overwrites.
        CFB_MILLIS_SOURCE.store(50, Ordering::SeqCst);
        assert_eq!(
            cfb_millis_fn(1, true),
            Ok(51),
            "Ok refresh recomputes + overwrites"
        );
        assert_eq!(CFB_MILLIS_CALLS.load(Ordering::SeqCst), 3);

        // Non-bypass call now serves the refreshed value.
        CFB_MILLIS_SOURCE.store(0, Ordering::SeqCst);
        assert_eq!(
            cfb_millis_fn(1, false),
            Ok(51),
            "serves the overwritten value"
        );
        assert_eq!(CFB_MILLIS_CALLS.load(Ordering::SeqCst), 3);
    }
}

// ── (#16/#140): in_impl caches a self-method ───────────────────────────────

static COMPUTE_CALLS: AtomicUsize = AtomicUsize::new(0);

struct Calculator {
    base: i32,
}

impl Calculator {
    #[cached(in_impl = true)]
    fn compute(&self, k: i32) -> i32 {
        COMPUTE_CALLS.fetch_add(1, Ordering::SeqCst);
        k * 2
    }
}

// Note: `COMPUTE_CALLS` and the in_impl cache static are owned exclusively by
// this test; the assertions depend on a fresh (empty) cache, which cannot be
// reset from here, so the test is left as-is.
#[test]
fn in_impl_self_method_caches() {
    let c = Calculator { base: 100 };
    assert_eq!(c.compute(5), 10);
    assert_eq!(c.compute(5), 10);
    assert_eq!(
        COMPUTE_CALLS.load(Ordering::SeqCst),
        1,
        "second call should hit cache"
    );
    assert_eq!(c.compute(6), 12);
    assert_eq!(COMPUTE_CALLS.load(Ordering::SeqCst), 2);
    // The cache is shared across instances (receiver is not part of the key).
    let other = Calculator { base: 0 };
    assert_eq!(other.compute(5), 10);
    assert_eq!(
        COMPUTE_CALLS.load(Ordering::SeqCst),
        2,
        "shared cache: still a hit"
    );
    let _ = c.base + other.base; // silence dead-code on `base`
}

// in_impl also works for `#[concurrent_cached]` and `#[once]` (sibling-method
// codegen): smoke-test that they compile and cache.

static CONC_METHOD_CALLS: AtomicUsize = AtomicUsize::new(0);
static ONCE_METHOD_CALLS: AtomicUsize = AtomicUsize::new(0);

struct Svc;

impl Svc {
    #[concurrent_cached(in_impl = true)]
    fn conc_method(&self, k: i32) -> i32 {
        CONC_METHOD_CALLS.fetch_add(1, Ordering::SeqCst);
        k + 1
    }

    #[once(in_impl = true)]
    fn once_method(&self, k: i32) -> i32 {
        ONCE_METHOD_CALLS.fetch_add(1, Ordering::SeqCst);
        k
    }
}

// Note: `CONC_METHOD_CALLS`/`ONCE_METHOD_CALLS` are reset below; the underlying
// in_impl cache statics are function-local and cannot be reset from here, so this
// test must remain the sole caller of `conc_method`/`once_method`.
#[test]
fn in_impl_concurrent_and_once_methods() {
    // Reset counters so the assertions do not depend on execution order.
    CONC_METHOD_CALLS.store(0, Ordering::SeqCst);
    ONCE_METHOD_CALLS.store(0, Ordering::SeqCst);

    let s = Svc;
    assert_eq!(s.conc_method(5), 6);
    assert_eq!(s.conc_method(5), 6);
    assert_eq!(CONC_METHOD_CALLS.load(Ordering::SeqCst), 1);

    assert_eq!(s.once_method(3), 3);
    assert_eq!(s.once_method(9), 3); // once: single value shared
    assert_eq!(ONCE_METHOD_CALLS.load(Ordering::SeqCst), 1);
}

// ── (#146 + #16/#140): force_refresh composes with in_impl ─────────────────
// The force_refresh guard is emitted inside the in_impl method body, so it must
// reference the method's own arguments rather than a free-function ident. Drive
// a keyed force_refresh on a `self`-method: a bypass call recomputes and
// overwrites the shared entry, and the next normal call reads the new value.

struct Refresher;

static IN_IMPL_FR_CALLS: AtomicUsize = AtomicUsize::new(0);
static IN_IMPL_FR_SOURCE: AtomicUsize = AtomicUsize::new(1);

impl Refresher {
    #[cached(
        in_impl = true,
        key = "i32",
        convert = "{ k }",
        force_refresh = "{ bypass }"
    )]
    fn load(&self, k: i32, bypass: bool) -> usize {
        IN_IMPL_FR_CALLS.fetch_add(1, Ordering::SeqCst);
        let _ = bypass; // consumed by the generated guard, not the body
        (k as usize) + IN_IMPL_FR_SOURCE.load(Ordering::SeqCst)
    }
}

// Note: `IN_IMPL_FR_CALLS` is reset below; the in_impl cache is function-local
// and cannot be reset from here, so this test must remain the sole caller of `load`.
#[test]
fn force_refresh_composes_with_in_impl() {
    // Reset the call counter (and source) so assertions are order-independent.
    IN_IMPL_FR_CALLS.store(0, Ordering::SeqCst);
    IN_IMPL_FR_SOURCE.store(1, Ordering::SeqCst);

    let r = Refresher;
    // miss → 1 + 1 = 2, cached under key 1
    assert_eq!(r.load(1, false), 2);
    assert_eq!(IN_IMPL_FR_CALLS.load(Ordering::SeqCst), 1);
    // hit: body not re-run
    assert_eq!(r.load(1, false), 2);
    assert_eq!(IN_IMPL_FR_CALLS.load(Ordering::SeqCst), 1);
    // bump the source, then force a refresh: body re-runs and overwrites key 1
    IN_IMPL_FR_SOURCE.store(100, Ordering::SeqCst);
    assert_eq!(r.load(1, true), 101);
    assert_eq!(IN_IMPL_FR_CALLS.load(Ordering::SeqCst), 2);
    // a subsequent normal call reads the refreshed entry (shared key, no footgun)
    assert_eq!(r.load(1, false), 101);
    assert_eq!(IN_IMPL_FR_CALLS.load(Ordering::SeqCst), 2);
}

// ── FIX 2a: #[cached(in_impl = true)] on a pub method ─────────────────────
// Pins that a public in_impl method compiles and actually caches (body runs
// exactly once for two same-arg calls).

struct PubImplStruct;

static PUB_IMPL_CALLS: AtomicUsize = AtomicUsize::new(0);

impl PubImplStruct {
    #[cached(in_impl = true)]
    pub fn pub_cached_method(&self, x: i32) -> i32 {
        PUB_IMPL_CALLS.fetch_add(1, Ordering::SeqCst);
        x * 3
    }
}

#[test]
fn in_impl_pub_method_caches() {
    PUB_IMPL_CALLS.store(0, Ordering::SeqCst);
    let s = PubImplStruct;
    assert_eq!(s.pub_cached_method(4), 12);
    assert_eq!(s.pub_cached_method(4), 12); // cache hit
    assert_eq!(
        PUB_IMPL_CALLS.load(Ordering::SeqCst),
        1,
        "second call with the same arg must be a cache hit"
    );
    assert_eq!(s.pub_cached_method(5), 15); // different key, miss
    assert_eq!(PUB_IMPL_CALLS.load(Ordering::SeqCst), 2);
}

// The `in_impl` macro generates a `{fn}_no_cache` sibling that bypasses the cache
// and always runs the body. Calling it after the cache is warm must increment the
// counter again, proving the body ran rather than returning the cached value.
//
// Uses its own struct/counter so it shares neither the function-local cache nor the
// call counter with `in_impl_pub_method_caches` (the two would otherwise race when
// the test harness runs them in parallel).
struct NoCacheSiblingStruct;

static NO_CACHE_SIBLING_CALLS: AtomicUsize = AtomicUsize::new(0);

impl NoCacheSiblingStruct {
    #[cached(in_impl = true)]
    pub fn cached_method(&self, x: i32) -> i32 {
        NO_CACHE_SIBLING_CALLS.fetch_add(1, Ordering::SeqCst);
        x * 3
    }
}

#[test]
fn in_impl_no_cache_sibling_bypasses_cache() {
    let s = NoCacheSiblingStruct;
    // Warm the cache for x=7 via the normal (cached) path.
    assert_eq!(s.cached_method(7), 21);
    assert_eq!(NO_CACHE_SIBLING_CALLS.load(Ordering::SeqCst), 1);
    // A second cached call is a hit; body does not run.
    assert_eq!(s.cached_method(7), 21);
    assert_eq!(NO_CACHE_SIBLING_CALLS.load(Ordering::SeqCst), 1);
    // The _no_cache sibling bypasses the cache; the body runs again.
    assert_eq!(s.cached_method_no_cache(7), 21);
    assert_eq!(
        NO_CACHE_SIBLING_CALLS.load(Ordering::SeqCst),
        2,
        "_no_cache sibling must bypass the cache and run the body"
    );
}

// ── FIX 2d: #[concurrent_cached(in_impl = true, force_refresh = "{ ... }")] ──
// Verifies that force_refresh composes with the concurrent in_impl path: a
// bypass call recomputes even when the entry is cached.

struct ConcImplRefresher;

static CONC_IMPL_FR_CALLS: AtomicUsize = AtomicUsize::new(0);
static CONC_IMPL_FR_SOURCE: AtomicUsize = AtomicUsize::new(1);

impl ConcImplRefresher {
    #[concurrent_cached(
        in_impl = true,
        key = "i32",
        convert = "{ k }",
        force_refresh = "{ bypass }"
    )]
    fn conc_impl_load(&self, k: i32, bypass: bool) -> usize {
        CONC_IMPL_FR_CALLS.fetch_add(1, Ordering::SeqCst);
        let _ = bypass; // consumed by the generated force_refresh guard
        (k as usize) + CONC_IMPL_FR_SOURCE.load(Ordering::SeqCst)
    }
}

#[test]
fn concurrent_in_impl_force_refresh_bypasses_cache() {
    CONC_IMPL_FR_CALLS.store(0, Ordering::SeqCst);
    CONC_IMPL_FR_SOURCE.store(1, Ordering::SeqCst);
    let r = ConcImplRefresher;
    // Miss: 2 + 1 = 3, cached under key 2.
    assert_eq!(r.conc_impl_load(2, false), 3);
    assert_eq!(CONC_IMPL_FR_CALLS.load(Ordering::SeqCst), 1);
    // Hit: body not re-run.
    assert_eq!(r.conc_impl_load(2, false), 3);
    assert_eq!(CONC_IMPL_FR_CALLS.load(Ordering::SeqCst), 1);
    // Force-refresh: body re-runs with updated source, overwrites entry.
    CONC_IMPL_FR_SOURCE.store(100, Ordering::SeqCst);
    assert_eq!(r.conc_impl_load(2, true), 102);
    assert_eq!(CONC_IMPL_FR_CALLS.load(Ordering::SeqCst), 2);
    // Subsequent normal call reads the refreshed entry.
    assert_eq!(r.conc_impl_load(2, false), 102);
    assert_eq!(CONC_IMPL_FR_CALLS.load(Ordering::SeqCst), 2);
}

// ── (#149): ttl_millis recompute (sub-second TTL) ──────────────────────────
// Gated on `time_stores` because the sub-second TTL store requires it.

#[cfg(feature = "time_stores")]
mod ttl_millis_tests {
    use super::*;
    use std::thread::sleep;
    use std::time::Duration;

    static MILLIS_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[cached(ttl_millis = 50)]
    fn millis_fn(x: i32) -> i32 {
        MILLIS_CALLS.fetch_add(1, Ordering::SeqCst);
        x
    }

    #[test]
    fn ttl_millis_recomputes_after_expiry() {
        MILLIS_CALLS.store(0, Ordering::SeqCst);
        assert_eq!(millis_fn(7), 7);
        assert_eq!(millis_fn(7), 7);
        assert_eq!(
            MILLIS_CALLS.load(Ordering::SeqCst),
            1,
            "within TTL: cache hit"
        );
        sleep(Duration::from_millis(70));
        assert_eq!(millis_fn(7), 7);
        assert_eq!(
            MILLIS_CALLS.load(Ordering::SeqCst),
            2,
            "after ttl_millis expiry: recompute"
        );
    }

    // ttl_millis on the `#[concurrent_cached]` default in-memory sharded path
    // (ShardedTtlCache): sub-second TTL is honored exactly in memory.
    static CONC_MILLIS_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[concurrent_cached(ttl_millis = 50)]
    fn conc_millis_fn(x: i32) -> i32 {
        CONC_MILLIS_CALLS.fetch_add(1, Ordering::SeqCst);
        x
    }

    #[test]
    fn concurrent_ttl_millis_recomputes_after_expiry() {
        CONC_MILLIS_CALLS.store(0, Ordering::SeqCst);
        assert_eq!(conc_millis_fn(7), 7);
        assert_eq!(conc_millis_fn(7), 7);
        assert_eq!(
            CONC_MILLIS_CALLS.load(Ordering::SeqCst),
            1,
            "within TTL: cache hit"
        );
        sleep(Duration::from_millis(70));
        assert_eq!(conc_millis_fn(7), 7);
        assert_eq!(
            CONC_MILLIS_CALLS.load(Ordering::SeqCst),
            2,
            "after ttl_millis expiry: recompute"
        );
    }

    // ttl_millis on `#[once]`: the single cached value expires sub-second and is
    // recomputed on the next call (the timestamped `Option` path, not a TtlCache).
    static ONCE_MILLIS_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[once(ttl_millis = 50)]
    fn once_millis_fn() -> usize {
        ONCE_MILLIS_CALLS.fetch_add(1, Ordering::SeqCst) + 1
    }

    #[test]
    fn once_ttl_millis_recomputes_after_expiry() {
        ONCE_MILLIS_CALLS.store(0, Ordering::SeqCst);
        // First call computes; the second is served from the single cached value.
        assert_eq!(once_millis_fn(), 1);
        assert_eq!(once_millis_fn(), 1);
        assert_eq!(
            ONCE_MILLIS_CALLS.load(Ordering::SeqCst),
            1,
            "within TTL: cache hit"
        );
        sleep(Duration::from_millis(70));
        // After the sub-second TTL expires the body re-runs, yielding the next value.
        assert_eq!(once_millis_fn(), 2);
        assert_eq!(
            ONCE_MILLIS_CALLS.load(Ordering::SeqCst),
            2,
            "after ttl_millis expiry: recompute"
        );
    }

    // ── FIX 2b: #[cached(in_impl = true, ttl_millis = N)] on a method ─────
    // The function-local timestamped static caches within the TTL and recomputes
    // after expiry, mirroring the free-function ttl_millis path but on a method.

    struct TtlImplStruct;

    static TTL_IMPL_CALLS: AtomicUsize = AtomicUsize::new(0);

    impl TtlImplStruct {
        #[cached(in_impl = true, ttl_millis = 50)]
        fn ttl_method(&self, x: i32) -> i32 {
            TTL_IMPL_CALLS.fetch_add(1, Ordering::SeqCst);
            x
        }
    }

    #[test]
    fn in_impl_ttl_millis_caches_and_recomputes() {
        TTL_IMPL_CALLS.store(0, Ordering::SeqCst);
        let s = TtlImplStruct;
        assert_eq!(s.ttl_method(9), 9);
        assert_eq!(s.ttl_method(9), 9);
        assert_eq!(
            TTL_IMPL_CALLS.load(Ordering::SeqCst),
            1,
            "within TTL: in_impl method must serve from cache"
        );
        sleep(Duration::from_millis(70));
        assert_eq!(s.ttl_method(9), 9);
        assert_eq!(
            TTL_IMPL_CALLS.load(Ordering::SeqCst),
            2,
            "after ttl_millis expiry: in_impl method must recompute"
        );
    }

    // ── FIX 2c: #[once(ttl_millis = N, force_refresh = "{ ... }")] ────────
    // `force_refresh` bypasses the single shared value before the TTL expires.
    // This is distinct from plain `#[once(ttl_millis)]` (expiry-driven recompute)
    // and plain `#[once(force_refresh)]` (no TTL): here both compose.

    static ONCE_TTL_FR_CALLS: AtomicUsize = AtomicUsize::new(0);
    static ONCE_TTL_FR_SOURCE: AtomicUsize = AtomicUsize::new(1);

    // Long TTL (600 s) so expiry does not drive any recompute during the test;
    // only the force_refresh bypass does.
    #[once(ttl_millis = 600_000, force_refresh = "{ bypass }")]
    fn once_ttl_fr(bypass: bool) -> usize {
        let _ = bypass; // consumed by the generated force_refresh guard
        ONCE_TTL_FR_CALLS.fetch_add(1, Ordering::SeqCst);
        ONCE_TTL_FR_SOURCE.load(Ordering::SeqCst)
    }

    #[test]
    fn once_ttl_millis_force_refresh_recomputes_before_expiry() {
        ONCE_TTL_FR_CALLS.store(0, Ordering::SeqCst);
        ONCE_TTL_FR_SOURCE.store(10, Ordering::SeqCst);
        // Miss: body runs, single value = 10 cached.
        assert_eq!(once_ttl_fr(false), 10);
        assert_eq!(ONCE_TTL_FR_CALLS.load(Ordering::SeqCst), 1);
        // Hit within TTL: body not re-run even though source changes.
        ONCE_TTL_FR_SOURCE.store(99, Ordering::SeqCst);
        assert_eq!(once_ttl_fr(false), 10, "within TTL: cached value returned");
        assert_eq!(ONCE_TTL_FR_CALLS.load(Ordering::SeqCst), 1);
        // Force-refresh before TTL expiry: body re-runs and overwrites.
        assert_eq!(once_ttl_fr(true), 99, "force_refresh recomputed new source");
        assert_eq!(ONCE_TTL_FR_CALLS.load(Ordering::SeqCst), 2);
        // Subsequent non-bypass call serves the refreshed value.
        assert_eq!(once_ttl_fr(false), 99, "later call sees overwritten value");
        assert_eq!(ONCE_TTL_FR_CALLS.load(Ordering::SeqCst), 2);
    }

    // ── #[cached(ttl_millis = N, force_refresh = "{ ... }")] ─────────────────
    // Covers the `#[cached]` path: force_refresh bypasses a cached entry before
    // the TTL expires, recomputes the body, and overwrites the slot. A subsequent
    // normal call confirms the overwritten value is served from the cache.

    static CACHED_TTL_FR_CALLS: AtomicUsize = AtomicUsize::new(0);
    static CACHED_TTL_FR_SOURCE: AtomicUsize = AtomicUsize::new(1);

    // Long TTL so only force_refresh (not expiry) drives any recompute during
    // the test. `bypass` is excluded from the key via `key`/`convert`.
    #[cached(
        key = "i32",
        convert = "{ x }",
        ttl_millis = 600_000,
        force_refresh = "{ bypass }"
    )]
    fn cached_ttl_fr(x: i32, bypass: bool) -> usize {
        let _ = bypass; // consumed by the generated force_refresh guard
        CACHED_TTL_FR_CALLS.fetch_add(1, Ordering::SeqCst);
        x as usize + CACHED_TTL_FR_SOURCE.load(Ordering::SeqCst)
    }

    #[test]
    fn cached_ttl_millis_force_refresh_recomputes_before_expiry() {
        CACHED_TTL_FR_CALLS.store(0, Ordering::SeqCst);
        CACHED_TTL_FR_SOURCE.store(1, Ordering::SeqCst);
        // Miss: 3 + 1 = 4, cached.
        assert_eq!(cached_ttl_fr(3, false), 4);
        assert_eq!(CACHED_TTL_FR_CALLS.load(Ordering::SeqCst), 1);
        // Hit within TTL: body not re-run even though source changes.
        CACHED_TTL_FR_SOURCE.store(100, Ordering::SeqCst);
        assert_eq!(
            cached_ttl_fr(3, false),
            4,
            "within TTL: cached value returned"
        );
        assert_eq!(CACHED_TTL_FR_CALLS.load(Ordering::SeqCst), 1);
        // Force-refresh before TTL expiry: body re-runs (3 + 100 = 103) and overwrites.
        assert_eq!(
            cached_ttl_fr(3, true),
            103,
            "force_refresh recomputed new source"
        );
        assert_eq!(CACHED_TTL_FR_CALLS.load(Ordering::SeqCst), 2);
        // Subsequent non-bypass call serves the refreshed (overwritten) value.
        assert_eq!(
            cached_ttl_fr(3, false),
            103,
            "later call sees overwritten value"
        );
        assert_eq!(CACHED_TTL_FR_CALLS.load(Ordering::SeqCst), 2);
    }

    // ── #[cached(max_size = N, ttl_millis = M)] (LruTtlCache path) ────────────
    // `max_size` + `ttl_millis` selects the bounded sub-second TTL store
    // (LruTtlCache). The entry caches within the TTL and recomputes after expiry.

    static LRU_MILLIS_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[cached(max_size = 10, ttl_millis = 50)]
    fn lru_millis_fn(x: i32) -> i32 {
        LRU_MILLIS_CALLS.fetch_add(1, Ordering::SeqCst);
        x
    }

    #[test]
    fn lru_ttl_millis_recomputes_after_expiry() {
        LRU_MILLIS_CALLS.store(0, Ordering::SeqCst);
        assert_eq!(lru_millis_fn(7), 7);
        assert_eq!(lru_millis_fn(7), 7);
        assert_eq!(
            LRU_MILLIS_CALLS.load(Ordering::SeqCst),
            1,
            "within TTL: cache hit"
        );
        sleep(Duration::from_millis(70));
        assert_eq!(lru_millis_fn(7), 7);
        assert_eq!(
            LRU_MILLIS_CALLS.load(Ordering::SeqCst),
            2,
            "after ttl_millis expiry: recompute"
        );
    }

    // ── #[concurrent_cached(in_impl = true, ttl_millis = N)] on a method ──────
    // Mirrors the `#[cached]` in_impl ttl_millis path on the concurrent macro:
    // the method caches within the TTL and recomputes after expiry.

    struct ConcTtlImplStruct;

    static CONC_TTL_IMPL_CALLS: AtomicUsize = AtomicUsize::new(0);

    impl ConcTtlImplStruct {
        #[concurrent_cached(in_impl = true, ttl_millis = 50)]
        fn ttl_method(&self, x: i32) -> i32 {
            CONC_TTL_IMPL_CALLS.fetch_add(1, Ordering::SeqCst);
            x
        }
    }

    #[test]
    fn concurrent_in_impl_ttl_millis_caches_and_recomputes() {
        CONC_TTL_IMPL_CALLS.store(0, Ordering::SeqCst);
        let s = ConcTtlImplStruct;
        assert_eq!(s.ttl_method(9), 9);
        assert_eq!(s.ttl_method(9), 9);
        assert_eq!(
            CONC_TTL_IMPL_CALLS.load(Ordering::SeqCst),
            1,
            "within TTL: in_impl method must serve from cache"
        );
        sleep(Duration::from_millis(70));
        assert_eq!(s.ttl_method(9), 9);
        assert_eq!(
            CONC_TTL_IMPL_CALLS.load(Ordering::SeqCst),
            2,
            "after ttl_millis expiry: in_impl method must recompute"
        );
    }

    // ── #[once(in_impl = true, ttl_millis = N)] on a method ───────────────────
    // The function-local timestamped single value caches within the TTL and
    // recomputes after expiry, on a `self`-method.

    struct OnceTtlImplStruct;

    static ONCE_TTL_IMPL_CALLS: AtomicUsize = AtomicUsize::new(0);

    impl OnceTtlImplStruct {
        #[once(in_impl = true, ttl_millis = 50)]
        fn ttl_method(&self) -> usize {
            ONCE_TTL_IMPL_CALLS.fetch_add(1, Ordering::SeqCst) + 1
        }
    }

    #[test]
    fn once_in_impl_ttl_millis_caches_and_recomputes() {
        ONCE_TTL_IMPL_CALLS.store(0, Ordering::SeqCst);
        let s = OnceTtlImplStruct;
        // First call computes; the second is served from the single cached value.
        assert_eq!(s.ttl_method(), 1);
        assert_eq!(s.ttl_method(), 1);
        assert_eq!(
            ONCE_TTL_IMPL_CALLS.load(Ordering::SeqCst),
            1,
            "within TTL: in_impl once method must serve the cached value"
        );
        sleep(Duration::from_millis(70));
        // After the sub-second TTL expires the body re-runs, yielding the next value.
        assert_eq!(s.ttl_method(), 2);
        assert_eq!(
            ONCE_TTL_IMPL_CALLS.load(Ordering::SeqCst),
            2,
            "after ttl_millis expiry: in_impl once method must recompute"
        );
    }

    // ── #[concurrent_cached(ttl_millis = N, force_refresh = "{ ... }")] ──────
    // The concurrent analogue of `cached_ttl_millis_force_refresh_recomputes_before_expiry`:
    // a long TTL keeps the entry fresh, and only the force_refresh bypass drives a
    // recompute, proving bypass recomputes within the TTL window.

    static CONC_TTL_FR_CALLS: AtomicUsize = AtomicUsize::new(0);
    static CONC_TTL_FR_SOURCE: AtomicUsize = AtomicUsize::new(1);

    #[concurrent_cached(
        key = "i32",
        convert = "{ x }",
        ttl_millis = 600_000,
        force_refresh = "{ bypass }"
    )]
    fn conc_ttl_fr(x: i32, bypass: bool) -> usize {
        let _ = bypass; // consumed by the generated force_refresh guard
        CONC_TTL_FR_CALLS.fetch_add(1, Ordering::SeqCst);
        x as usize + CONC_TTL_FR_SOURCE.load(Ordering::SeqCst)
    }

    #[test]
    fn concurrent_ttl_millis_force_refresh_recomputes_before_expiry() {
        CONC_TTL_FR_CALLS.store(0, Ordering::SeqCst);
        CONC_TTL_FR_SOURCE.store(1, Ordering::SeqCst);
        // Miss: 3 + 1 = 4, cached.
        assert_eq!(conc_ttl_fr(3, false), 4);
        assert_eq!(CONC_TTL_FR_CALLS.load(Ordering::SeqCst), 1);
        // Hit within TTL: body not re-run even though source changes.
        CONC_TTL_FR_SOURCE.store(100, Ordering::SeqCst);
        assert_eq!(
            conc_ttl_fr(3, false),
            4,
            "within TTL: cached value returned"
        );
        assert_eq!(CONC_TTL_FR_CALLS.load(Ordering::SeqCst), 1);
        // Force-refresh before TTL expiry: body re-runs (3 + 100 = 103) and overwrites.
        assert_eq!(
            conc_ttl_fr(3, true),
            103,
            "force_refresh recomputed new source"
        );
        assert_eq!(CONC_TTL_FR_CALLS.load(Ordering::SeqCst), 2);
        // Subsequent non-bypass call serves the refreshed (overwritten) value.
        assert_eq!(
            conc_ttl_fr(3, false),
            103,
            "later call sees overwritten value"
        );
        assert_eq!(CONC_TTL_FR_CALLS.load(Ordering::SeqCst), 2);
    }
}

// ── TTL spellings: `ttl` (Duration expr), `ttl_secs`, `ttl_millis` ─────────
// The 3-way ttl API exposes the same underlying time-based TTL store through
// three attribute spellings. These tests prove each spelling actually caches a
// hit and then recomputes after the TTL expires, on every macro (in-memory
// path). `ttl` and `ttl_millis` use a sub-second duration so expiry is fast;
// `ttl_secs` uses the 1 s minimum and waits just past it.
#[cfg(feature = "time_stores")]
mod ttl_spelling_tests {
    use super::*;
    use std::thread::sleep;
    use std::time::Duration;

    // ── `ttl = "Duration::from_millis(50)"` (the Duration-expression form) ──
    static TTL_EXPR_CACHED_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[cached(ttl = "core::time::Duration::from_millis(50)")]
    fn ttl_expr_cached(x: i32) -> i32 {
        TTL_EXPR_CACHED_CALLS.fetch_add(1, Ordering::SeqCst);
        x
    }

    #[test]
    fn ttl_expr_cached_recomputes_after_expiry() {
        TTL_EXPR_CACHED_CALLS.store(0, Ordering::SeqCst);
        assert_eq!(ttl_expr_cached(7), 7);
        assert_eq!(ttl_expr_cached(7), 7);
        assert_eq!(
            TTL_EXPR_CACHED_CALLS.load(Ordering::SeqCst),
            1,
            "within TTL: cache hit"
        );
        sleep(Duration::from_millis(70));
        assert_eq!(ttl_expr_cached(7), 7);
        assert_eq!(
            TTL_EXPR_CACHED_CALLS.load(Ordering::SeqCst),
            2,
            "after `ttl` Duration expiry: recompute"
        );
    }

    static TTL_EXPR_ONCE_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[once(ttl = "core::time::Duration::from_millis(50)")]
    fn ttl_expr_once() -> usize {
        TTL_EXPR_ONCE_CALLS.fetch_add(1, Ordering::SeqCst) + 1
    }

    #[test]
    fn ttl_expr_once_recomputes_after_expiry() {
        TTL_EXPR_ONCE_CALLS.store(0, Ordering::SeqCst);
        assert_eq!(ttl_expr_once(), 1);
        assert_eq!(ttl_expr_once(), 1);
        assert_eq!(
            TTL_EXPR_ONCE_CALLS.load(Ordering::SeqCst),
            1,
            "within TTL: cache hit"
        );
        sleep(Duration::from_millis(70));
        assert_eq!(ttl_expr_once(), 2);
        assert_eq!(
            TTL_EXPR_ONCE_CALLS.load(Ordering::SeqCst),
            2,
            "after `ttl` Duration expiry: recompute"
        );
    }

    static TTL_EXPR_CONC_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[concurrent_cached(ttl = "core::time::Duration::from_millis(50)")]
    fn ttl_expr_conc(x: i32) -> i32 {
        TTL_EXPR_CONC_CALLS.fetch_add(1, Ordering::SeqCst);
        x
    }

    #[test]
    fn ttl_expr_concurrent_recomputes_after_expiry() {
        TTL_EXPR_CONC_CALLS.store(0, Ordering::SeqCst);
        assert_eq!(ttl_expr_conc(7), 7);
        assert_eq!(ttl_expr_conc(7), 7);
        assert_eq!(
            TTL_EXPR_CONC_CALLS.load(Ordering::SeqCst),
            1,
            "within TTL: cache hit"
        );
        sleep(Duration::from_millis(70));
        assert_eq!(ttl_expr_conc(7), 7);
        assert_eq!(
            TTL_EXPR_CONC_CALLS.load(Ordering::SeqCst),
            2,
            "after `ttl` Duration expiry: recompute"
        );
    }

    // ── `ttl_secs = 1` (whole-seconds form; 1 s is the minimum) ────────────
    static TTL_SECS_CACHED_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[cached(ttl_secs = 1)]
    fn ttl_secs_cached(x: i32) -> i32 {
        TTL_SECS_CACHED_CALLS.fetch_add(1, Ordering::SeqCst);
        x
    }

    #[test]
    fn ttl_secs_cached_recomputes_after_expiry() {
        TTL_SECS_CACHED_CALLS.store(0, Ordering::SeqCst);
        assert_eq!(ttl_secs_cached(7), 7);
        assert_eq!(ttl_secs_cached(7), 7);
        assert_eq!(
            TTL_SECS_CACHED_CALLS.load(Ordering::SeqCst),
            1,
            "within TTL: cache hit"
        );
        sleep(Duration::from_millis(1_100));
        assert_eq!(ttl_secs_cached(7), 7);
        assert_eq!(
            TTL_SECS_CACHED_CALLS.load(Ordering::SeqCst),
            2,
            "after `ttl_secs` expiry: recompute"
        );
    }

    static TTL_SECS_ONCE_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[once(ttl_secs = 1)]
    fn ttl_secs_once() -> usize {
        TTL_SECS_ONCE_CALLS.fetch_add(1, Ordering::SeqCst) + 1
    }

    #[test]
    fn ttl_secs_once_recomputes_after_expiry() {
        TTL_SECS_ONCE_CALLS.store(0, Ordering::SeqCst);
        assert_eq!(ttl_secs_once(), 1);
        assert_eq!(ttl_secs_once(), 1);
        assert_eq!(
            TTL_SECS_ONCE_CALLS.load(Ordering::SeqCst),
            1,
            "within TTL: cache hit"
        );
        sleep(Duration::from_millis(1_100));
        assert_eq!(ttl_secs_once(), 2);
        assert_eq!(
            TTL_SECS_ONCE_CALLS.load(Ordering::SeqCst),
            2,
            "after `ttl_secs` expiry: recompute"
        );
    }

    static TTL_SECS_CONC_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[concurrent_cached(ttl_secs = 1)]
    fn ttl_secs_conc(x: i32) -> i32 {
        TTL_SECS_CONC_CALLS.fetch_add(1, Ordering::SeqCst);
        x
    }

    #[test]
    fn ttl_secs_concurrent_recomputes_after_expiry() {
        TTL_SECS_CONC_CALLS.store(0, Ordering::SeqCst);
        assert_eq!(ttl_secs_conc(7), 7);
        assert_eq!(ttl_secs_conc(7), 7);
        assert_eq!(
            TTL_SECS_CONC_CALLS.load(Ordering::SeqCst),
            1,
            "within TTL: cache hit"
        );
        sleep(Duration::from_millis(1_100));
        assert_eq!(ttl_secs_conc(7), 7);
        assert_eq!(
            TTL_SECS_CONC_CALLS.load(Ordering::SeqCst),
            2,
            "after `ttl_secs` expiry: recompute"
        );
    }

    // ── `ttl_millis = 50` (millisecond form) on all three macros ───────────
    // (The dedicated `ttl_millis_tests` module also covers this; these mirror
    // the `ttl`/`ttl_secs` cases so all three spellings sit side by side.)
    static TTL_MILLIS_CACHED_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[cached(ttl_millis = 50)]
    fn ttl_millis_cached(x: i32) -> i32 {
        TTL_MILLIS_CACHED_CALLS.fetch_add(1, Ordering::SeqCst);
        x
    }

    #[test]
    fn ttl_millis_cached_recomputes_after_expiry() {
        TTL_MILLIS_CACHED_CALLS.store(0, Ordering::SeqCst);
        assert_eq!(ttl_millis_cached(7), 7);
        assert_eq!(ttl_millis_cached(7), 7);
        assert_eq!(
            TTL_MILLIS_CACHED_CALLS.load(Ordering::SeqCst),
            1,
            "within TTL: cache hit"
        );
        sleep(Duration::from_millis(70));
        assert_eq!(ttl_millis_cached(7), 7);
        assert_eq!(
            TTL_MILLIS_CACHED_CALLS.load(Ordering::SeqCst),
            2,
            "after `ttl_millis` expiry: recompute"
        );
    }

    static TTL_MILLIS_ONCE_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[once(ttl_millis = 50)]
    fn ttl_millis_once() -> usize {
        TTL_MILLIS_ONCE_CALLS.fetch_add(1, Ordering::SeqCst) + 1
    }

    #[test]
    fn ttl_millis_once_recomputes_after_expiry() {
        TTL_MILLIS_ONCE_CALLS.store(0, Ordering::SeqCst);
        assert_eq!(ttl_millis_once(), 1);
        assert_eq!(ttl_millis_once(), 1);
        assert_eq!(
            TTL_MILLIS_ONCE_CALLS.load(Ordering::SeqCst),
            1,
            "within TTL: cache hit"
        );
        sleep(Duration::from_millis(70));
        assert_eq!(ttl_millis_once(), 2);
        assert_eq!(
            TTL_MILLIS_ONCE_CALLS.load(Ordering::SeqCst),
            2,
            "after `ttl_millis` expiry: recompute"
        );
    }

    static TTL_MILLIS_CONC_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[concurrent_cached(ttl_millis = 50)]
    fn ttl_millis_conc(x: i32) -> i32 {
        TTL_MILLIS_CONC_CALLS.fetch_add(1, Ordering::SeqCst);
        x
    }

    #[test]
    fn ttl_millis_concurrent_recomputes_after_expiry() {
        TTL_MILLIS_CONC_CALLS.store(0, Ordering::SeqCst);
        assert_eq!(ttl_millis_conc(7), 7);
        assert_eq!(ttl_millis_conc(7), 7);
        assert_eq!(
            TTL_MILLIS_CONC_CALLS.load(Ordering::SeqCst),
            1,
            "within TTL: cache hit"
        );
        sleep(Duration::from_millis(70));
        assert_eq!(ttl_millis_conc(7), 7);
        assert_eq!(
            TTL_MILLIS_CONC_CALLS.load(Ordering::SeqCst),
            2,
            "after `ttl_millis` expiry: recompute"
        );
    }
}

// ── async in_impl: #[once(in_impl = true)] on an async self-method ─────────
// `#[once]` stores one shared value for all callers. On an async in_impl method
// the body must run exactly once across repeated awaits with the same receiver.

#[cfg(feature = "async")]
mod async_in_impl_tests {
    use super::*;

    struct AsyncSvc;

    static ASYNC_ONCE_CALLS: AtomicUsize = AtomicUsize::new(0);

    impl AsyncSvc {
        #[once(in_impl = true)]
        async fn load(&self, x: i32) -> i32 {
            ASYNC_ONCE_CALLS.fetch_add(1, Ordering::SeqCst);
            x * 2
        }
    }

    #[tokio::test]
    async fn async_in_impl_once_caches_across_awaits() {
        ASYNC_ONCE_CALLS.store(0, Ordering::SeqCst);
        let s = AsyncSvc;
        // First await computes and caches the single shared value.
        assert_eq!(s.load(5).await, 10);
        // Later awaits (even with a different arg) serve the single cached value;
        // the body runs exactly once.
        assert_eq!(
            s.load(7).await,
            10,
            "once: single value shared across awaits"
        );
        assert_eq!(
            ASYNC_ONCE_CALLS.load(Ordering::SeqCst),
            1,
            "async in_impl once: body runs exactly once"
        );
    }

    // ── async in_impl: #[cached(in_impl = true)] on an async method ─────────
    // The keyed cache stores a separate entry per argument. The body runs once per
    // unique key and subsequent awaits with the same arg serve from the cache.

    struct AsyncCachedSvc;

    static ASYNC_CACHED_CALLS: AtomicUsize = AtomicUsize::new(0);

    impl AsyncCachedSvc {
        #[cached(in_impl = true)]
        async fn compute(&self, x: i32) -> i32 {
            ASYNC_CACHED_CALLS.fetch_add(1, Ordering::SeqCst);
            x * 3
        }
    }

    // Note: `ASYNC_CACHED_CALLS` is reset below; the in_impl cache is function-local
    // and cannot be reset from here, so this test must remain the sole caller of `compute`.
    #[tokio::test]
    async fn async_in_impl_cached_caches_per_key() {
        ASYNC_CACHED_CALLS.store(0, Ordering::SeqCst);
        let s = AsyncCachedSvc;
        // First await for x=4: miss, body runs, result cached.
        assert_eq!(s.compute(4).await, 12);
        assert_eq!(ASYNC_CACHED_CALLS.load(Ordering::SeqCst), 1);
        // Second await with the same arg: cache hit, body not re-run.
        assert_eq!(s.compute(4).await, 12);
        assert_eq!(
            ASYNC_CACHED_CALLS.load(Ordering::SeqCst),
            1,
            "async in_impl cached: second await with same arg must be a cache hit"
        );
        // Different arg: new key, body runs again.
        assert_eq!(s.compute(5).await, 15);
        assert_eq!(ASYNC_CACHED_CALLS.load(Ordering::SeqCst), 2);
    }

    // ── async in_impl: #[concurrent_cached(in_impl = true)] on an async method ──
    // The concurrent sharded cache stores a separate entry per argument. The body
    // runs once per unique key and subsequent awaits serve from the cache.

    struct AsyncConcSvc;

    static ASYNC_CONC_CALLS: AtomicUsize = AtomicUsize::new(0);

    impl AsyncConcSvc {
        #[concurrent_cached(in_impl = true)]
        async fn fetch(&self, x: i32) -> i32 {
            ASYNC_CONC_CALLS.fetch_add(1, Ordering::SeqCst);
            x + 10
        }
    }

    // Note: `ASYNC_CONC_CALLS` is reset below; the in_impl cache is function-local
    // and cannot be reset from here, so this test must remain the sole caller of `fetch`.
    #[tokio::test]
    async fn async_in_impl_concurrent_caches_per_key() {
        ASYNC_CONC_CALLS.store(0, Ordering::SeqCst);
        let s = AsyncConcSvc;
        // First await for x=7: miss, body runs, result cached.
        assert_eq!(s.fetch(7).await, 17);
        assert_eq!(ASYNC_CONC_CALLS.load(Ordering::SeqCst), 1);
        // Second await with the same arg: cache hit, body not re-run.
        assert_eq!(s.fetch(7).await, 17);
        assert_eq!(
            ASYNC_CONC_CALLS.load(Ordering::SeqCst),
            1,
            "async in_impl concurrent_cached: second await with same arg must be a cache hit"
        );
        // Different arg: new key, body runs again.
        assert_eq!(s.fetch(3).await, 13);
        assert_eq!(ASYNC_CONC_CALLS.load(Ordering::SeqCst), 2);
    }
}
