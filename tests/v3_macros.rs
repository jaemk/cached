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

    // ── #[concurrent_cached(refresh = true, ttl_millis = N)] ─────────────────
    // Behavioral smoke-test: `refresh = true` is now a plain `bool` (not
    // `Option<bool>`). The cache compiles and caches correctly. `refresh = false`
    // (the default) is the baseline; `refresh = true` also caches correctly and the
    // store is constructed with `refresh_on_hit(true)`. We verify caching behavior
    // on the plain `refresh = false` path here; the TTL-renewal side effect of
    // `refresh_on_hit(true)` cannot be tested without sleeping past the TTL again.

    static REFRESH_CONC_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[concurrent_cached(ttl_millis = 600_000, refresh = true)]
    fn conc_refresh_fn(x: i32) -> i32 {
        REFRESH_CONC_CALLS.fetch_add(1, Ordering::SeqCst);
        x
    }

    #[test]
    fn concurrent_cached_refresh_bool_compiles_and_caches() {
        REFRESH_CONC_CALLS.store(0, Ordering::SeqCst);
        // First call: miss, body runs.
        assert_eq!(conc_refresh_fn(7), 7);
        assert_eq!(REFRESH_CONC_CALLS.load(Ordering::SeqCst), 1);
        // Second call: cache hit, body does not re-run.
        assert_eq!(conc_refresh_fn(7), 7);
        assert_eq!(
            REFRESH_CONC_CALLS.load(Ordering::SeqCst),
            1,
            "refresh = true (bool) still caches: second call must be a hit"
        );
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

// ── FIX B: #[once(sync_writes, force_refresh)] predicate evaluated once ──────
// Before the fix, `do_set_return_block` in the `SyncWriteMode::Default` arm
// expanded the force_refresh predicate TWICE: once inside the read-lock block
// and again in the write-lock re-check. A side-effecting predicate therefore
// ran twice on every write path (cache miss or bypass call).
//
// After the fix, the predicate is hoisted into a single `__cached_force_refreshing`
// binding before both checks, so it is evaluated AT MOST ONCE per call.
//
// This test uses a predicate that always returns `false` (never force-refresh)
// and increments a counter as a side effect. On a cache miss (first call), the
// write path is taken; pre-fix the counter reaches 2, post-fix it stays at 1.

static ONCE_SW_FR_PRED_COUNT: AtomicUsize = AtomicUsize::new(0);
static ONCE_SW_FR_BODY_COUNT: AtomicUsize = AtomicUsize::new(0);

// NOTE: must be call-exclusive to `once_sync_writes_force_refresh_predicate_eval_count`.
// The cache static is module-global (not in_impl) and cannot be reset, so no
// other test may call this function.
#[once(
    sync_writes,
    force_refresh = "{ ONCE_SW_FR_PRED_COUNT.fetch_add(1, Ordering::SeqCst); false }"
)]
fn once_sync_writes_fr(x: usize) -> usize {
    ONCE_SW_FR_BODY_COUNT.fetch_add(1, Ordering::SeqCst);
    x
}

#[test]
fn once_sync_writes_force_refresh_predicate_eval_count() {
    ONCE_SW_FR_PRED_COUNT.store(0, Ordering::SeqCst);
    ONCE_SW_FR_BODY_COUNT.store(0, Ordering::SeqCst);

    // First call: cache miss. The write path is taken.
    // Pre-fix: predicate runs in the read-lock block AND in the write-lock
    // re-check => ONCE_SW_FR_PRED_COUNT would be 2.
    // Post-fix: predicate is hoisted into a single binding => count == 1.
    let _ = once_sync_writes_fr(42);
    assert_eq!(
        ONCE_SW_FR_BODY_COUNT.load(Ordering::SeqCst),
        1,
        "body must run exactly once on a cache miss"
    );
    assert_eq!(
        ONCE_SW_FR_PRED_COUNT.load(Ordering::SeqCst),
        1,
        "force_refresh predicate must be evaluated EXACTLY ONCE per call, not twice (#FIX-B)"
    );

    // Second call: cache warm, force_refresh returns false => served from cache.
    // The predicate runs once more (from the read-lock path).
    let _ = once_sync_writes_fr(42);
    assert_eq!(
        ONCE_SW_FR_BODY_COUNT.load(Ordering::SeqCst),
        1,
        "body must not run again on a cache hit"
    );
    assert_eq!(
        ONCE_SW_FR_PRED_COUNT.load(Ordering::SeqCst),
        2,
        "predicate evaluated once per call (2 calls total)"
    );
}

// ── FIX C: default-key Option<&mut T> does not move the argument ─────────────
// Before the fix, the default-key path for `Option<&mut T>` emitted
// `name.map(|__cached_v| __cached_v.to_owned())`, which MOVES `name`. The
// generated `_no_cache` call then tried to reuse `name` after the move, causing
// a compile error.
//
// After the fix, `name.as_deref().map(|__cached_v| __cached_v.to_owned())` is
// emitted. `as_deref()` takes `&self` without consuming the Option, so `name`
// remains usable.

static OPT_MUT_REF_BODY_COUNT: AtomicUsize = AtomicUsize::new(0);

#[cached]
fn opt_mut_ref_cached(s: Option<&mut String>) -> usize {
    OPT_MUT_REF_BODY_COUNT.fetch_add(1, Ordering::SeqCst);
    s.as_deref().map_or(0, |v| v.len())
}

#[test]
fn opt_mut_ref_default_key_compiles_and_caches() {
    OPT_MUT_REF_BODY_COUNT.store(0, Ordering::SeqCst);

    // Two calls with equal keys (same string content) must hit the cache on the
    // second call: the body should run exactly once.
    let mut a = String::from("hello");
    let mut b = String::from("hello");
    let r1 = opt_mut_ref_cached(Some(&mut a));
    let r2 = opt_mut_ref_cached(Some(&mut b));
    assert_eq!(r1, 5);
    assert_eq!(r2, 5);
    assert_eq!(
        OPT_MUT_REF_BODY_COUNT.load(Ordering::SeqCst),
        1,
        "Option<&mut String> with equal keys: body must run exactly once (cache hit on second call)"
    );

    // A call with a different key must miss.
    let mut c = String::from("world!");
    let r3 = opt_mut_ref_cached(Some(&mut c));
    assert_eq!(r3, 6);
    assert_eq!(OPT_MUT_REF_BODY_COUNT.load(Ordering::SeqCst), 2);

    // None key must also be cacheable.
    let r4 = opt_mut_ref_cached(None);
    let r5 = opt_mut_ref_cached(None);
    assert_eq!(r4, 0);
    assert_eq!(r5, 0);
    assert_eq!(
        OPT_MUT_REF_BODY_COUNT.load(Ordering::SeqCst),
        3,
        "None key: body runs once, second call is a cache hit"
    );
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

// ── #5: the `unbound` attribute is removed; plain `#[cached]` is the default
//        unbounded store. These tests lock the behavior that replaced the
//        removed attribute: a bare `#[cached]` (no max_size/ttl/expires)
//        produces a working unbounded cache. `#[cached(unbound)]` is now a
//        compile error, covered by the `cached_unbound_attr_removed` trybuild
//        golden; here we prove the positive replacement behavior.
mod unbound_default_tests {
    use super::*;

    // Each test uses its own `#[cached]` fn (hence its own cache static and
    // counter) so the two tests never share a cache or counter across the
    // single test binary. Counts are asserted as absolute values that hold for
    // the sole caller of each function.

    static UNBOUND_REPEAT_CALLS: AtomicUsize = AtomicUsize::new(0);

    // No `max_size`, `ttl`, or `expires`: the default store is an `UnboundCache`.
    #[cached]
    fn unbound_repeat(x: u32) -> u32 {
        UNBOUND_REPEAT_CALLS.fetch_add(1, Ordering::SeqCst);
        x * 2
    }

    #[test]
    fn plain_cached_caches_repeated_same_arg() {
        // First call for x=21: miss, body runs.
        assert_eq!(unbound_repeat(21), 42);
        assert_eq!(UNBOUND_REPEAT_CALLS.load(Ordering::SeqCst), 1);
        // Repeated same-arg call: cache hit, body does not re-run. This is the
        // behavior the removed `unbound` attribute used to opt into and is now
        // the `#[cached]` default.
        assert_eq!(unbound_repeat(21), 42);
        assert_eq!(
            UNBOUND_REPEAT_CALLS.load(Ordering::SeqCst),
            1,
            "plain #[cached] (no unbound attr) must cache repeated same-arg calls"
        );
    }

    static UNBOUND_FILL_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[cached]
    fn unbound_fill(x: u32) -> u32 {
        UNBOUND_FILL_CALLS.fetch_add(1, Ordering::SeqCst);
        x * 2
    }

    #[test]
    fn plain_cached_is_unbounded_no_eviction() {
        // Insert many distinct keys, far more than any LRU default would retain.
        for i in 100..1100u32 {
            assert_eq!(unbound_fill(i), i * 2);
        }
        let after_fill = UNBOUND_FILL_CALLS.load(Ordering::SeqCst);
        assert_eq!(after_fill, 1000, "1000 distinct keys each computed once");
        // The very first key inserted must still be cached (unbounded: no
        // eviction). A bounded store would have evicted it by now and the body
        // would re-run, bumping the counter.
        assert_eq!(unbound_fill(100), 200);
        assert_eq!(
            UNBOUND_FILL_CALLS.load(Ordering::SeqCst),
            after_fill,
            "default #[cached] is unbounded: the earliest key is never evicted"
        );
    }
}

// ── #8: `#[concurrent_cached]` `refresh` is now a plain `bool` (parity with
//        `#[cached]`). `refresh = false` is the default and no longer trips the
//        expires+refresh or refresh+create conflict checks (previously
//        `refresh = Some(false)` made those combinations a compile error). These
//        are compile-and-behavior tests locking that `refresh = false` is inert.
mod refresh_false_no_conflict_tests {
    use super::*;

    // Per-value expiry store payload: implements `Expires`. Used to prove
    // `refresh = false` does NOT conflict with `expires = true` (the conflict
    // only fires for `refresh = true`).
    #[derive(Clone)]
    struct NeverExpires(u32);

    impl cached::Expires for NeverExpires {
        fn is_expired(&self) -> bool {
            false
        }
    }

    static REFRESH_FALSE_EXPIRES_CALLS: AtomicUsize = AtomicUsize::new(0);

    // `refresh = false` + `expires = true`: would have been a hard conflict when
    // `refresh` was `Option<bool>` and `Some(false)` was set. Now compiles and
    // behaves as a plain expires cache.
    #[concurrent_cached(expires = true, refresh = false)]
    fn refresh_false_expires(x: u32) -> NeverExpires {
        REFRESH_FALSE_EXPIRES_CALLS.fetch_add(1, Ordering::SeqCst);
        NeverExpires(x)
    }

    #[test]
    fn refresh_false_does_not_conflict_with_expires() {
        REFRESH_FALSE_EXPIRES_CALLS.store(0, Ordering::SeqCst);
        assert_eq!(refresh_false_expires(9).0, 9);
        assert_eq!(REFRESH_FALSE_EXPIRES_CALLS.load(Ordering::SeqCst), 1);
        // Cache hit: body not re-run. The value never expires.
        assert_eq!(refresh_false_expires(9).0, 9);
        assert_eq!(
            REFRESH_FALSE_EXPIRES_CALLS.load(Ordering::SeqCst),
            1,
            "refresh = false + expires = true must compile and cache"
        );
    }
}

// ── Item 2 positive guard: a VALID `name` still compiles and caches ──────────
// The `name` validation rejects non-identifier strings (see the
// `*_name_invalid_ident` trybuild fixtures). This guard proves the validation
// did not over-reject: a legal Rust identifier in `name` produces a working
// cache static under that exact name and memoizes across calls.

static VALID_NAME_CALLS: AtomicUsize = AtomicUsize::new(0);

#[cached(name = "MY_CACHE")]
fn valid_name_caches(x: u32) -> u32 {
    VALID_NAME_CALLS.fetch_add(1, Ordering::SeqCst);
    x + 1
}

#[test]
fn valid_name_compiles_and_caches() {
    VALID_NAME_CALLS.store(0, Ordering::SeqCst);

    // First call for key 5: cache miss, body runs.
    assert_eq!(valid_name_caches(5), 6);
    assert_eq!(VALID_NAME_CALLS.load(Ordering::SeqCst), 1);

    // Repeat of key 5: cache hit, body must not run.
    assert_eq!(valid_name_caches(5), 6);
    assert_eq!(
        VALID_NAME_CALLS.load(Ordering::SeqCst),
        1,
        "a valid `name` must produce a working memoizing cache"
    );

    // A different key is a distinct entry: body runs once more.
    assert_eq!(valid_name_caches(10), 11);
    assert_eq!(VALID_NAME_CALLS.load(Ordering::SeqCst), 2);

    // The cache static is named exactly `MY_CACHE` (proves `name` took effect).
    // If the identifier were not honored this reference would not resolve.
    use cached::Cached;
    assert!(MY_CACHE.read().cache_size() >= 2);
}

// ── Item 9 positive guard: `sync_writes` is STILL valid on `#[once]` ─────────
// Item 9 rejects `sync_lock`/`unsync_reads` on `#[once]`, but `sync_writes`
// (and `sync_writes = "default"`/`= true`) must remain accepted because they
// drive `#[once]` codegen. These guards prove the rejection did not over-reach.
// Each function's cache static is module-global and cannot be reset, so each is
// call-exclusive to its own test.

static ONCE_SW_DEFAULT_CALLS: AtomicUsize = AtomicUsize::new(0);

#[once(sync_writes = "default")]
fn once_sync_writes_default(x: usize) -> usize {
    ONCE_SW_DEFAULT_CALLS.fetch_add(1, Ordering::SeqCst);
    x * 2
}

#[test]
fn once_sync_writes_default_compiles_and_caches() {
    ONCE_SW_DEFAULT_CALLS.store(0, Ordering::SeqCst);

    // First call: cache miss, body runs.
    assert_eq!(once_sync_writes_default(21), 42);
    assert_eq!(ONCE_SW_DEFAULT_CALLS.load(Ordering::SeqCst), 1);

    // `#[once]` stores a single value for ALL arguments: a different argument
    // still returns the first cached value and does not re-run the body.
    assert_eq!(once_sync_writes_default(100), 42);
    assert_eq!(
        ONCE_SW_DEFAULT_CALLS.load(Ordering::SeqCst),
        1,
        "`sync_writes = \"default\"` on `#[once]` must still compile and cache the one value"
    );
}

static ONCE_SW_TRUE_CALLS: AtomicUsize = AtomicUsize::new(0);

#[once(sync_writes = true)]
fn once_sync_writes_true(x: usize) -> usize {
    ONCE_SW_TRUE_CALLS.fetch_add(1, Ordering::SeqCst);
    x + 7
}

#[test]
fn once_sync_writes_true_compiles_and_caches() {
    ONCE_SW_TRUE_CALLS.store(0, Ordering::SeqCst);

    assert_eq!(once_sync_writes_true(1), 8);
    assert_eq!(ONCE_SW_TRUE_CALLS.load(Ordering::SeqCst), 1);

    // Single shared value: a hit on any later call.
    assert_eq!(once_sync_writes_true(999), 8);
    assert_eq!(
        ONCE_SW_TRUE_CALLS.load(Ordering::SeqCst),
        1,
        "`sync_writes = true` on `#[once]` must still compile and cache"
    );
}

// ── Item #1 (reverted): bare #[cached] defaults to Disabled sync_writes ───────
//
// The default `sync_writes` was reverted from ByKey back to `Disabled` (2.x
// behavior): no write synchronization. A ByKey default deadlocks recursive
// memoized fns (the per-key bucket lock is held across the body). This is pinned
// via recursion: `bare_cached_recursion_does_not_deadlock` (sync) and
// `bare_cached_async_recursion_does_not_deadlock` (async) in tests/cached.rs.
// ByKey (per-key bucket lock) or Default (global write lock) held across the body
// would deadlock those; only Disabled does not, which rules out both non-Disabled
// defaults. (The static shape no longer distinguishes the modes: the ByKey static
// is now a `KeyedCache` that derefs to the inner cache lock, so `.read()`/`.write()`
// compile uniformly across sync_writes modes by design.)

// Counter proves the body ran exactly once across sequential miss+hit calls.
static CACHED_DEFAULT_DISABLED_CALLS: AtomicUsize = AtomicUsize::new(0);

#[cached(key = "u32", convert = { k })]
fn cached_default_disabled(k: u32) -> u32 {
    CACHED_DEFAULT_DISABLED_CALLS.fetch_add(1, Ordering::SeqCst);
    k * 2
}

#[test]
fn test_cached_default_is_disabled_not_by_key() {
    use cached::Cached;
    CACHED_DEFAULT_DISABLED_CALLS.store(0, Ordering::SeqCst);

    // Miss then hit: the bare default must still cache.
    assert_eq!(cached_default_disabled(1), 2);
    // Named-static inspection via `.read()` works for every sync_writes mode (the ByKey static
    // is a `KeyedCache` that derefs to the inner cache lock), so this exercises the documented
    // inspection path; the deadlock guard above is what pins the default to Disabled.
    let hits_before = CACHED_DEFAULT_DISABLED.read().cache_hits();
    assert_eq!(cached_default_disabled(1), 2); // cache hit
    let hits_after = CACHED_DEFAULT_DISABLED.read().cache_hits();
    assert!(
        hits_after > hits_before,
        "bare #[cached] must still cache by default"
    );
    // The body ran exactly once: first call missed and computed, second hit.
    assert_eq!(CACHED_DEFAULT_DISABLED_CALLS.load(Ordering::SeqCst), 1);
}

// A `sync_writes = "by_key"` named static is inspected with the same `.read()`/`.write()` as any
// other generated static: the static is a doc-hidden `KeyedCache` that derefs to the inner cache
// lock, hiding the bucket vector. This is the MACRO-8 fix (previously the static was a tuple and
// inspection required `.0.read()`).
#[cached(sync_writes = "by_key", sync_writes_buckets = 8)]
fn by_key_inspectable(x: u32) -> u32 {
    x * 2
}

#[test]
fn by_key_named_static_inspectable_via_read_and_write() {
    use cached::{Cached, CachedRead};

    assert_eq!(by_key_inspectable(2), 4);
    assert_eq!(by_key_inspectable(3), 6);

    // `.read()` on the static (no `.0` tuple access) gives shared access for read-only inspection.
    {
        let guard = BY_KEY_INSPECTABLE.read();
        assert_eq!(CachedRead::cache_get_read(&*guard, &2), Some(&4));
        assert_eq!(CachedRead::cache_get_read(&*guard, &3), Some(&6));
    }
    // `.write()` gives exclusive access; clear via the same handle.
    {
        let mut guard = BY_KEY_INSPECTABLE.write();
        assert_eq!(guard.cache_get(&2), Some(&4));
        guard.cache_clear();
    }
    assert_eq!(
        CachedRead::cache_get_read(&*BY_KEY_INSPECTABLE.read(), &2),
        None
    );

    // by_key still deduplicates and caches correctly after the manual clear.
    assert_eq!(by_key_inspectable(2), 4);
    assert!(BY_KEY_INSPECTABLE.write().cache_get(&2).is_some());
}

// sync_writes = false restores the old Disabled behavior: concurrent threads
// for the same key each compute independently (race possible, but no dedup).
static CACHED_SW_FALSE_CALLS: AtomicUsize = AtomicUsize::new(0);

#[cached(key = "u32", convert = { k }, sync_writes = false)]
fn cached_sw_false(k: u32) -> u32 {
    CACHED_SW_FALSE_CALLS.fetch_add(1, Ordering::SeqCst);
    k * 3
}

#[test]
fn test_cached_sync_writes_false_double_compute() {
    // With sync_writes = false (Disabled) the static is a plain RwLock.
    // Verify the static type is not a tuple by accessing it directly.
    CACHED_SW_FALSE_CALLS.store(0, Ordering::SeqCst);
    assert_eq!(cached_sw_false(5), 15);
    // Reading the lock directly (not .0) proves the static is the bare lock type,
    // which is only true when sync_writes is Disabled.
    use cached::Cached;
    let hits_before = CACHED_SW_FALSE.read().cache_hits();
    assert_eq!(cached_sw_false(5), 15); // cache hit
    let hits_after = CACHED_SW_FALSE.read().cache_hits();
    assert!(
        hits_after > hits_before,
        "sync_writes = false: cache should hit on repeated call"
    );
}

// result_fallback = true on a bare #[cached] must NOT error; it silently selects
// Disabled sync_writes (since ByKey is incompatible with result_fallback).
// This test verifies the combination compiles and caches correctly.
#[cfg(feature = "time_stores")]
static CACHED_RF_NO_SW_CALLS: AtomicUsize = AtomicUsize::new(0);

#[cfg(feature = "time_stores")]
#[cached(ttl_secs = 3600, result_fallback = true, key = "u32", convert = { k })]
fn cached_result_fallback_no_sync_writes(k: u32) -> Result<u32, String> {
    CACHED_RF_NO_SW_CALLS.fetch_add(1, Ordering::SeqCst);
    Ok(k)
}

#[cfg(feature = "time_stores")]
#[test]
fn test_cached_result_fallback_no_explicit_sync_writes_compiles() {
    CACHED_RF_NO_SW_CALLS.store(0, Ordering::SeqCst);
    let v = cached_result_fallback_no_sync_writes(7).unwrap();
    assert_eq!(v, 7);
    let cached_v = cached_result_fallback_no_sync_writes(7).unwrap();
    assert_eq!(cached_v, 7);
    // Body runs only once (second call is a cache hit).
    assert_eq!(CACHED_RF_NO_SW_CALLS.load(Ordering::SeqCst), 1);
}

// ── Item #2: unquoted syn::Expr for code-valued attributes ───────────────────

// Unquoted `convert = { format!("{a}") }` (no quotes around the block).
static UNQUOTED_CONVERT_CALLS: AtomicUsize = AtomicUsize::new(0);

#[cached(key = "String", convert = { format!("{a}") })]
fn unquoted_convert(a: u32) -> u32 {
    UNQUOTED_CONVERT_CALLS.fetch_add(1, Ordering::SeqCst);
    a
}

#[test]
fn test_cached_unquoted_convert_compiles_and_caches() {
    UNQUOTED_CONVERT_CALLS.store(0, Ordering::SeqCst);
    assert_eq!(unquoted_convert(3), 3);
    assert_eq!(unquoted_convert(3), 3); // cache hit
    assert_eq!(UNQUOTED_CONVERT_CALLS.load(Ordering::SeqCst), 1);
}

// Unquoted `create`: a bare expression (previously panicked the macro) and a
// single-expression block (previously tripped `unused_braces` in value position)
// must both compile and cache. The example `kitchen_sink` is the `-D warnings`
// regression guard for the lint; this guards the parse/cache behavior.
#[cached(ty = "cached::UnboundCache<u32, u32>", create = cached::UnboundCache::new())]
fn unquoted_create_bare(x: u32) -> u32 {
    x + 1
}

#[cached(ty = "cached::UnboundCache<u32, u32>", create = { cached::UnboundCache::new() })]
fn unquoted_create_block(x: u32) -> u32 {
    x + 1
}

#[test]
fn test_cached_unquoted_create_forms_compile_and_cache() {
    assert_eq!(unquoted_create_bare(1), 2);
    assert_eq!(unquoted_create_bare(1), 2); // cache hit
    assert_eq!(unquoted_create_block(1), 2);
    assert_eq!(unquoted_create_block(1), 2); // cache hit
}

// Legacy quoted `convert = "{ n + 1 }"` must still work.
static QUOTED_CONVERT_CALLS: AtomicUsize = AtomicUsize::new(0);

#[cached(key = "u32", convert = "{ n + 1 }")]
fn quoted_convert(n: u32) -> u32 {
    QUOTED_CONVERT_CALLS.fetch_add(1, Ordering::SeqCst);
    n
}

#[test]
fn test_cached_legacy_quoted_convert_compiles_and_caches() {
    QUOTED_CONVERT_CALLS.store(0, Ordering::SeqCst);
    assert_eq!(quoted_convert(10), 10);
    assert_eq!(quoted_convert(10), 10); // cache hit — same key (n+1=11)
    assert_eq!(QUOTED_CONVERT_CALLS.load(Ordering::SeqCst), 1);
}

// Unquoted `force_refresh = { k == 0 }` must compile and bypass the cache when
// the predicate evaluates to true.
static UNQUOTED_FR_CALLS: AtomicUsize = AtomicUsize::new(0);
static UNQUOTED_FR_SRC: AtomicUsize = AtomicUsize::new(42);

// convert = { k % 100 } maps all keys to a small set of cache slots;
// force_refresh = { k == 0 } bypasses the cache when k is zero.
// The body also reads k (via UNQUOTED_FR_SRC) to suppress unused-variable lint.
#[cached(key = "u32", convert = { k % 100 }, force_refresh = { k == 0 })]
fn unquoted_force_refresh(k: u32) -> u32 {
    UNQUOTED_FR_CALLS.fetch_add(1, Ordering::SeqCst);
    // Use k to ensure the body produces a key-dependent result.
    UNQUOTED_FR_SRC.load(Ordering::SeqCst) as u32 + (k % 100)
}

#[test]
fn test_cached_unquoted_force_refresh_compiles_and_works() {
    UNQUOTED_FR_CALLS.store(0, Ordering::SeqCst);
    UNQUOTED_FR_SRC.store(10, Ordering::SeqCst);

    // k == 1 => force_refresh = false: normal caching; body returns src(10) + 1%100 = 11.
    assert_eq!(unquoted_force_refresh(1), 11);
    assert_eq!(unquoted_force_refresh(1), 11); // cache hit
    assert_eq!(UNQUOTED_FR_CALLS.load(Ordering::SeqCst), 1);

    // Change underlying source.
    UNQUOTED_FR_SRC.store(99, Ordering::SeqCst);

    // k == 0 => force_refresh = true: bypasses cache, re-runs body; returns src(99) + 0%100 = 99.
    assert_eq!(unquoted_force_refresh(0), 99);
    assert_eq!(UNQUOTED_FR_CALLS.load(Ordering::SeqCst), 2);
}

// ── Item #3: map_error optional on fallible concurrent paths ─────────────────

// A disk-backed concurrent_cached function whose error type implements
// From<RedbCacheError> must compile without an explicit `map_error` closure.
// The macro generates `.map_err(Into::into)?` automatically.
// Use Box<dyn Error> as the error type: its From<RedbCacheError> impl is
// unambiguous because the blanket `From<E: Error> for Box<dyn Error>` is the
// only applicable conversion.
#[cfg(all(feature = "redb_store", feature = "proc_macro"))]
mod disk_no_map_error_tests {
    use cached::macros::concurrent_cached;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static DISK_NO_MAP_ERR_CALLS: AtomicUsize = AtomicUsize::new(0);

    // No `map_error` attribute: the macro generates `Into::into` implicitly.
    // `Box<dyn std::error::Error + Send + Sync>` implements `From<RedbCacheError>`
    // via the blanket `From<E: Error + Send + Sync>`, giving unambiguous inference.
    #[concurrent_cached(disk = true, ttl_secs = 60)]
    fn disk_fn_no_map_error(n: u32) -> Result<u32, Box<dyn std::error::Error + Send + Sync>> {
        DISK_NO_MAP_ERR_CALLS.fetch_add(1, Ordering::SeqCst);
        Ok(n * 2)
    }

    #[test]
    fn test_disk_concurrent_without_map_error_compiles_and_caches() {
        // The primary assertion: the function compiles and returns the correct value.
        // The disk (redb) cache is persistent across test runs, so the body may or may
        // not run on any given run. We verify correctness of the return value only.
        assert_eq!(
            disk_fn_no_map_error(3).unwrap(),
            6,
            "disk_fn_no_map_error(3) must return Ok(6)"
        );
        // A second call must also return the correct value (either from cache or body).
        assert_eq!(
            disk_fn_no_map_error(3).unwrap(),
            6,
            "disk_fn_no_map_error(3) repeated call must return Ok(6)"
        );
    }
}

// ── Item #9: companions_vis knob ─────────────────────────────────────────────

// Verify that `companions_vis = "pub(crate)"` causes the no_cache companion
// and prime_cache companion to be generated with `pub(crate)` visibility.
// We test this indirectly: if companions_vis works the test module can call
// the companion fn that would otherwise be private or less visible.
mod companions_vis_tests {
    use cached::macros::cached;

    // pub fn with companions_vis = "pub(crate)": the companion
    // `test_companions_vis_fn_no_cache` must be callable from this module.
    #[cached(key = "u32", convert = { n }, companions_vis = "pub(crate)")]
    pub fn companions_vis_fn(n: u32) -> u32 {
        n * 7
    }

    #[test]
    fn test_companions_vis_pub_crate_produces_pub_crate_companions() {
        // Call the no_cache companion directly — this only compiles if it is
        // pub(crate) (or more visible). If companions_vis is not respected the
        // companion would be `pub` (matching the fn), but here we check it is
        // accessible, which is the positive signal.
        let direct = companions_vis_fn_no_cache(2);
        assert_eq!(
            direct, 14,
            "companions_vis: no_cache companion returned wrong value"
        );
    }

    // Default (no companions_vis): companion inherits the fn's visibility.
    #[cached(key = "u32", convert = { n })]
    pub fn default_companions_vis_fn(n: u32) -> u32 {
        n + 1
    }

    #[test]
    fn test_companions_vis_default_inherits_fn_visibility() {
        // The no_cache companion should be callable (pub inherited).
        let direct = default_companions_vis_fn_no_cache(5);
        assert_eq!(
            direct, 6,
            "default companions_vis: no_cache companion returned wrong value"
        );
    }
}

// ── Item #9b: companions_vis on #[once] ──────────────────────────────────────

// For free functions `#[once]` nests the `_no_cache` origin inside the cached
// fn body (it cannot be a module-level companion because it has no per-call key
// and the cache is a single shared static). The only module-level companion
// that carries `companions_vis` on the free-function path is `_prime_cache`.
// These tests verify that `companions_vis` is honoured for that companion.
mod companions_vis_once_tests {
    use cached::macros::once;

    // `companions_vis = "pub(crate)"`: the `_prime_cache` companion must be
    // callable from inside this module. If the knob is ignored the companion
    // would get the fn's own visibility (`pub`), but reachability from the test
    // is the positive signal regardless.
    #[once(companions_vis = "pub(crate)")]
    pub fn companions_vis_once_fn() -> u32 {
        21
    }

    #[test]
    fn test_companions_vis_once_pub_crate_produces_pub_crate_prime_cache() {
        // Calling `_prime_cache` directly only compiles if it is pub(crate) or
        // more visible. The function always runs the body, so the return value
        // must match.
        let val = companions_vis_once_fn_prime_cache();
        assert_eq!(
            val, 21,
            "companions_vis on #[once]: prime_cache companion returned wrong value"
        );
    }

    // Default (no companions_vis): companion inherits the fn's own visibility.
    #[once]
    pub fn default_companions_vis_once_fn() -> u32 {
        22
    }

    #[test]
    fn test_companions_vis_once_default_inherits_fn_visibility() {
        // With no `companions_vis` the companion is `pub` (matching the fn).
        // Calling it from this sibling test confirms it is accessible.
        let val = default_companions_vis_once_fn_prime_cache();
        assert_eq!(
            val, 22,
            "default companions_vis on #[once]: prime_cache companion returned wrong value"
        );
    }
}

// ── Item #9c: companions_vis on #[concurrent_cached] ─────────────────────────

// `#[concurrent_cached]` also nests `_no_cache` inside the cached fn body for
// free functions. The module-level companion that carries `companions_vis` is
// `_prime_cache`. These tests verify the knob is honoured for that macro.
mod companions_vis_concurrent_tests {
    use cached::macros::concurrent_cached;

    // `companions_vis = "pub(crate)"`: the `_prime_cache` companion must be
    // callable from inside this module.
    #[concurrent_cached(key = "u32", convert = { n }, companions_vis = "pub(crate)")]
    pub fn companions_vis_concurrent_fn(n: u32) -> u32 {
        n * 11
    }

    #[test]
    fn test_companions_vis_concurrent_pub_crate_produces_pub_crate_prime_cache() {
        // Calling `_prime_cache` directly only compiles if it is pub(crate) or
        // more visible. `_prime_cache` always runs the body and stores the
        // result, so the return value must match `n * 11`.
        let val = companions_vis_concurrent_fn_prime_cache(3);
        assert_eq!(
            val, 33,
            "companions_vis on #[concurrent_cached]: prime_cache companion returned wrong value"
        );
    }

    // Default (no companions_vis): companion inherits the fn's own visibility.
    #[concurrent_cached(key = "u32", convert = { n })]
    pub fn default_companions_vis_concurrent_fn(n: u32) -> u32 {
        n + 10
    }

    #[test]
    fn test_companions_vis_concurrent_default_inherits_fn_visibility() {
        let val = default_companions_vis_concurrent_fn_prime_cache(7);
        assert_eq!(
            val, 17,
            "default companions_vis on #[concurrent_cached]: prime_cache companion returned wrong value"
        );
    }
}

// ── G1 positive: generic `#[once]` with a concrete value type ────────────────
// A generic `#[once]` function is valid as long as the return type does not name
// any of the function's own type or const parameters. The guard added in G1
// must not affect this case.

static CONCRETE_ONCE_CALLS: AtomicUsize = AtomicUsize::new(0);

// `T` is used only as an input parameter; the return type `usize` is concrete.
#[once]
fn generic_once_concrete_return<T: std::fmt::Debug>(_x: T) -> usize {
    CONCRETE_ONCE_CALLS.fetch_add(1, Ordering::SeqCst);
    42
}

#[test]
fn generic_once_concrete_value_type_compiles_and_caches() {
    CONCRETE_ONCE_CALLS.store(0, Ordering::SeqCst);
    // First call: body runs.
    assert_eq!(generic_once_concrete_return::<i32>(1), 42);
    assert_eq!(CONCRETE_ONCE_CALLS.load(Ordering::SeqCst), 1);
    // Second call with different type/arg: cached hit, body does not re-run.
    assert_eq!(
        generic_once_concrete_return::<String>("hello".to_string()),
        42
    );
    assert_eq!(
        CONCRETE_ONCE_CALLS.load(Ordering::SeqCst),
        1,
        "#[once] with concrete return type: subsequent calls must be cache hits"
    );
}

// ── G2 positive: valid custom `name` on `#[once]` still works ────────────────
// A `name` that does NOT begin with `__cached` must compile and be usable as
// the cache static identifier on `#[once]` (the G2 guard must not over-reject).

static NAMED_ONCE_CALLS: AtomicUsize = AtomicUsize::new(0);

#[once(name = "MY_CUSTOM_ONCE_CACHE")]
fn named_once_fn() -> usize {
    NAMED_ONCE_CALLS.fetch_add(1, Ordering::SeqCst);
    99
}

#[test]
fn once_valid_name_compiles_and_caches() {
    NAMED_ONCE_CALLS.store(0, Ordering::SeqCst);
    assert_eq!(named_once_fn(), 99);
    assert_eq!(NAMED_ONCE_CALLS.load(Ordering::SeqCst), 1);
    assert_eq!(named_once_fn(), 99);
    assert_eq!(
        NAMED_ONCE_CALLS.load(Ordering::SeqCst),
        1,
        "valid custom name on #[once]: second call must be a cache hit"
    );
}
