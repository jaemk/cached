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
