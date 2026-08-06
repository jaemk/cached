/*!
Coverage for the `companions` attribute on `#[cached]`, `#[once]`, and
`#[concurrent_cached]`.

`companions = false` suppresses the generated `{fn}_no_cache` and `{fn}_prime_cache`
free functions so they cannot collide with the user's own items in the parent module.
The default (attribute omitted, or `companions = true`) is unchanged.

The positive tests below prove the cached function still memoizes with the companions
gone; the default-behavior tests prove the switch is opt-in by calling both companions;
and the trybuild cases prove the companions really are absent rather than merely
undocumented.

`in_impl = true` composes with `companions = false` rather than conflicting: `in_impl`
already suppresses `{fn}_prime_cache`, and the `{fn}_no_cache` origin has to stay a
sibling `impl` method because it takes a `self` receiver, which a function-local `fn`
cannot. An inherent method is namespaced by its type and cannot collide with a free
function in the parent module, so nothing is left to suppress.
*/

#![cfg(feature = "proc_macro")]

use std::sync::atomic::{AtomicUsize, Ordering};

use cached::macros::{cached, concurrent_cached, once};

// ── #[cached(companions = false)] still memoizes ──────────────────────────────

static CACHED_CALLS: AtomicUsize = AtomicUsize::new(0);

#[cached(companions = false)]
fn cached_no_companions(x: u32) -> u32 {
    CACHED_CALLS.fetch_add(1, Ordering::SeqCst);
    x * 2
}

#[test]
fn cached_companions_false_still_caches() {
    CACHED_CALLS.store(0, Ordering::SeqCst);

    assert_eq!(cached_no_companions(4), 8);
    assert_eq!(cached_no_companions(4), 8);
    assert_eq!(
        CACHED_CALLS.load(Ordering::SeqCst),
        1,
        "the body runs once per key; the origin fn moved into the cached fn body"
    );

    // A second key still reaches the (now function-local) origin.
    assert_eq!(cached_no_companions(5), 10);
    assert_eq!(CACHED_CALLS.load(Ordering::SeqCst), 2);
}

// A `?`-carrying body proves the origin is still a real `fn` after being nested:
// inlining the block into the wrapper would rebind `?` and `return` to the wrapper.
#[cached(companions = false)]
fn cached_no_companions_try(x: u32) -> Result<u32, String> {
    if x == 0 {
        return Err("zero".to_string());
    }
    let doubled = u32::try_from(u64::from(x) * 2).map_err(|e| e.to_string())?;
    Ok(doubled)
}

#[test]
fn cached_companions_false_preserves_early_return_and_try() {
    assert_eq!(cached_no_companions_try(3), Ok(6));
    assert_eq!(cached_no_companions_try(0), Err("zero".to_string()));
}

// ── generated code must not put trait names in a scope enclosing user code ────
//
// The wrapper block encloses user-written tokens: the `convert` and `force_refresh`
// expressions always, and the whole function body once `companions = false` nests the
// origin fn. A `use ::cached::Cached;` emitted into that block would be a nearer scope
// than the user's module, so it would shadow a same-named user item and turn
// `Cached::marker(x)` into a reference to the trait (E0782). The generated code names
// every trait through a fully-qualified path instead, so these compile.
//
// The types below deliberately reuse the names of the traits the codegen calls:
// `Cached` (every store path) and `CloneCached` (the `result_fallback` path).

/// Shadows the `cached::Cached` trait name.
struct Cached {
    value: u32,
}

impl Cached {
    fn marker(value: u32) -> Self {
        Self { value }
    }
}

/// Shadows the `cached::CloneCached` trait name.
struct CloneCached {
    value: u32,
}

impl CloneCached {
    fn marker(value: u32) -> Self {
        Self { value }
    }
}

#[cached(companions = false)]
fn body_defines_own_cached_item(x: u32) -> u32 {
    let local = Cached::marker(x);
    local.value * 2
}

#[cached(companions = false)]
fn body_defines_own_clone_cached_item(x: u32) -> u32 {
    let local = CloneCached::marker(x);
    local.value * 3
}

#[test]
fn companions_false_does_not_shadow_user_items_named_after_traits() {
    assert_eq!(body_defines_own_cached_item(4), 8);
    assert_eq!(body_defines_own_cached_item(4), 8);
    assert_eq!(body_defines_own_clone_cached_item(4), 12);
}

// The same exposure exists on the default (module-scope origin) path through the
// `convert` and `force_refresh` expressions, which are expanded into the wrapper block
// whether or not the origin is nested.
#[cached(
    companions = false,
    key = "u32",
    convert = "{ Cached::marker(x).value }",
    force_refresh = "{ CloneCached::marker(x).value == 0 }"
)]
fn key_exprs_use_user_items_named_after_traits(x: u32) -> u32 {
    x * 5
}

#[cached(
    key = "u32",
    convert = "{ Cached::marker(x).value }",
    force_refresh = "{ CloneCached::marker(x).value == 0 }"
)]
fn default_key_exprs_use_user_items_named_after_traits(x: u32) -> u32 {
    x * 6
}

/// `result_fallback` is the path that needed `CloneCached` in scope; its `convert`
/// expression must still resolve the user's item of the same name.
#[cfg(feature = "time_stores")]
#[cached(
    ttl_secs = 60,
    result_fallback = true,
    key = "u32",
    convert = "{ CloneCached::marker(x).value }"
)]
fn result_fallback_key_expr_uses_user_item(x: u32) -> Result<u32, String> {
    Ok(x * 7)
}

#[test]
fn user_items_named_after_traits_resolve_in_key_and_refresh_exprs() {
    // A non-zero argument keeps the `force_refresh` predicate false, so the second
    // call is served from the cache.
    assert_eq!(key_exprs_use_user_items_named_after_traits(3), 15);
    assert_eq!(key_exprs_use_user_items_named_after_traits(3), 15);
    assert_eq!(default_key_exprs_use_user_items_named_after_traits(3), 18);
    assert_eq!(default_key_exprs_use_user_items_named_after_traits(3), 18);
    #[cfg(feature = "time_stores")]
    {
        assert_eq!(result_fallback_key_expr_uses_user_item(3), Ok(21));
        assert_eq!(result_fallback_key_expr_uses_user_item(3), Ok(21));
    }
}

// ── the default still emits both companions ───────────────────────────────────

static DEFAULT_CALLS: AtomicUsize = AtomicUsize::new(0);

#[cached]
fn cached_with_companions(x: u32) -> u32 {
    DEFAULT_CALLS.fetch_add(1, Ordering::SeqCst);
    x * 3
}

#[test]
fn cached_default_emits_both_companions() {
    DEFAULT_CALLS.store(0, Ordering::SeqCst);

    // `_no_cache` bypasses the cache entirely.
    assert_eq!(cached_with_companions_no_cache(2), 6);
    assert_eq!(DEFAULT_CALLS.load(Ordering::SeqCst), 1);

    // `_prime_cache` runs the body and stores the result.
    assert_eq!(cached_with_companions_prime_cache(2), 6);
    assert_eq!(DEFAULT_CALLS.load(Ordering::SeqCst), 2);

    // The primed entry is now served without running the body.
    assert_eq!(cached_with_companions(2), 6);
    assert_eq!(DEFAULT_CALLS.load(Ordering::SeqCst), 2);
}

// An explicit `companions = true` is the same as omitting it.
#[cached(companions = true)]
fn cached_companions_true(x: u32) -> u32 {
    x + 1
}

#[test]
fn cached_companions_true_emits_both_companions() {
    assert_eq!(cached_companions_true_no_cache(1), 2);
    assert_eq!(cached_companions_true_prime_cache(1), 2);
    assert_eq!(cached_companions_true(1), 2);
}

// ── #[once(companions = false)] ───────────────────────────────────────────────

static ONCE_CALLS: AtomicUsize = AtomicUsize::new(0);

#[once(companions = false)]
fn once_no_companions(x: u32) -> u32 {
    ONCE_CALLS.fetch_add(1, Ordering::SeqCst);
    x * 10
}

#[test]
fn once_companions_false_still_caches() {
    ONCE_CALLS.store(0, Ordering::SeqCst);

    assert_eq!(once_no_companions(2), 20);
    // `#[once]` stores one value for all arguments.
    assert_eq!(once_no_companions(9), 20);
    assert_eq!(ONCE_CALLS.load(Ordering::SeqCst), 1);
}

static ONCE_DEFAULT_CALLS: AtomicUsize = AtomicUsize::new(0);

#[once]
fn once_with_companions(x: u32) -> u32 {
    ONCE_DEFAULT_CALLS.fetch_add(1, Ordering::SeqCst);
    x * 100
}

#[test]
fn once_default_emits_prime_companion() {
    ONCE_DEFAULT_CALLS.store(0, Ordering::SeqCst);

    assert_eq!(once_with_companions_prime_cache(3), 300);
    assert_eq!(ONCE_DEFAULT_CALLS.load(Ordering::SeqCst), 1);
    assert_eq!(once_with_companions(7), 300, "the primed value is served");
    assert_eq!(ONCE_DEFAULT_CALLS.load(Ordering::SeqCst), 1);
}

// ── #[concurrent_cached(companions = false)] ──────────────────────────────────

static CONCURRENT_CALLS: AtomicUsize = AtomicUsize::new(0);

#[concurrent_cached(companions = false)]
fn concurrent_no_companions(x: u32) -> u32 {
    CONCURRENT_CALLS.fetch_add(1, Ordering::SeqCst);
    x * 4
}

#[test]
fn concurrent_cached_companions_false_still_caches() {
    CONCURRENT_CALLS.store(0, Ordering::SeqCst);

    assert_eq!(concurrent_no_companions(6), 24);
    assert_eq!(concurrent_no_companions(6), 24);
    assert_eq!(CONCURRENT_CALLS.load(Ordering::SeqCst), 1);
}

static CONCURRENT_DEFAULT_CALLS: AtomicUsize = AtomicUsize::new(0);

#[concurrent_cached]
fn concurrent_with_companions(x: u32) -> u32 {
    CONCURRENT_DEFAULT_CALLS.fetch_add(1, Ordering::SeqCst);
    x * 5
}

#[test]
fn concurrent_cached_default_emits_prime_companion() {
    CONCURRENT_DEFAULT_CALLS.store(0, Ordering::SeqCst);

    assert_eq!(concurrent_with_companions_prime_cache(2), 10);
    assert_eq!(CONCURRENT_DEFAULT_CALLS.load(Ordering::SeqCst), 1);
    assert_eq!(concurrent_with_companions(2), 10);
    assert_eq!(CONCURRENT_DEFAULT_CALLS.load(Ordering::SeqCst), 1);
}

// ── companions = false composes with in_impl = true ───────────────────────────

static IN_IMPL_CALLS: AtomicUsize = AtomicUsize::new(0);

struct Service;

impl Service {
    // `in_impl` already suppresses `_prime_cache`; `companions = false` adds nothing
    // more to suppress here, because the `_no_cache` origin must stay a sibling method
    // to keep its `self` receiver. The pairing compiles and caches rather than erroring.
    #[cached(in_impl = true, companions = false)]
    fn compute(&self, x: u32) -> u32 {
        IN_IMPL_CALLS.fetch_add(1, Ordering::SeqCst);
        x * 7
    }
}

#[test]
fn cached_in_impl_with_companions_false_caches_and_keeps_origin_sibling() {
    IN_IMPL_CALLS.store(0, Ordering::SeqCst);
    let service = Service;

    assert_eq!(service.compute(2), 14);
    assert_eq!(service.compute(2), 14);
    assert_eq!(
        IN_IMPL_CALLS.load(Ordering::SeqCst),
        1,
        "the in_impl cache still memoizes with companions = false"
    );

    // The origin sibling survives under `in_impl` (it holds the `self`-taking body)
    // and stays callable as the documented escape hatch.
    assert_eq!(service.compute_no_cache(2), 14);
    assert_eq!(IN_IMPL_CALLS.load(Ordering::SeqCst), 2);
}

// ── the companions really are absent ──────────────────────────────────────────

#[test]
fn compile_fail_companions_optout() {
    let t = trybuild::TestCases::new();
    // `companions = false` removes both `#[cached]` companions from the module.
    t.compile_fail("tests/ui/cached_companions_false_no_cache_absent.rs");
    t.compile_fail("tests/ui/cached_companions_false_prime_absent.rs");
    // `#[once]` / `#[concurrent_cached]` nest the origin already, so `_prime_cache`
    // is the free function each of them drops.
    t.compile_fail("tests/ui/once_companions_false_prime_absent.rs");
    t.compile_fail("tests/ui/concurrent_cached_companions_false_prime_absent.rs");
    // `companions_vis` has nothing left to apply to and is rejected rather than
    // silently ignored.
    t.compile_fail("tests/ui/cached_companions_vis_with_companions_false.rs");
    t.compile_fail("tests/ui/once_companions_vis_with_companions_false.rs");
    t.compile_fail("tests/ui/concurrent_cached_companions_vis_with_companions_false.rs");
}
