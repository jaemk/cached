/*!
Regression tests: `name = "r#<keyword>"` must not panic the proc macro.

Prior to the fix, `syn::parse_str::<syn::Ident>("r#type")` succeeds (raw idents are
valid `Ident` tokens), but the subsequent `Ident::new("r#type", span)` panics because
`proc_macro2::Ident::new` rejects strings starting with `r#`. The fix uses
`Ident::new_raw` for the stripped name when a `r#` prefix is present.

The tests below prove that:
1. Functions with raw-identifier cache names compile and memoize correctly.
2. The generated static is reachable under its raw-identifier name.
3. A genuinely invalid name still produces the existing compile error (covered by the
   existing trybuild golden in `tests/ui/cached_name_invalid_ident.rs`).
*/

#![cfg(feature = "proc_macro")]
// The raw-identifier cache statics (`r#type`, `r#match`, `r#fn`) are lowercase because
// they shadow Rust keywords; suppress the uppercase-static lint for this file.
#![allow(non_upper_case_globals)]

use std::sync::atomic::{AtomicUsize, Ordering};

use cached::macros::{cached, concurrent_cached, once};

// ── #[cached] with a raw-identifier name ─────────────────────────────────────

static CACHED_TYPE_CALLS: AtomicUsize = AtomicUsize::new(0);

// `type` is a Rust keyword -- only reachable as a raw identifier.
#[cached(name = "r#type")]
fn cached_raw_ident_type(x: u32) -> u32 {
    CACHED_TYPE_CALLS.fetch_add(1, Ordering::SeqCst);
    x + 1
}

#[test]
fn cached_raw_ident_name_compiles_and_memoizes() {
    CACHED_TYPE_CALLS.store(0, Ordering::SeqCst);

    // First call: cache miss, body runs.
    assert_eq!(cached_raw_ident_type(5), 6);
    assert_eq!(CACHED_TYPE_CALLS.load(Ordering::SeqCst), 1);

    // Same key: cache hit, body must not run again.
    assert_eq!(cached_raw_ident_type(5), 6);
    assert_eq!(
        CACHED_TYPE_CALLS.load(Ordering::SeqCst),
        1,
        "#[cached(name = \"r#type\")]: second call with same key must be a cache hit"
    );

    // Different key: distinct entry, body runs once more.
    assert_eq!(cached_raw_ident_type(10), 11);
    assert_eq!(CACHED_TYPE_CALLS.load(Ordering::SeqCst), 2);

    // The static is accessible under the raw identifier `r#type`, proving the
    // identifier was generated correctly (if the name were wrong this would not compile).
    use cached::Cached;
    assert!(
        r#type.read().cache_size() >= 2,
        "cache static r#type must be reachable and contain at least 2 entries"
    );
}

// ── #[once] with a raw-identifier name ───────────────────────────────────────

static ONCE_MATCH_CALLS: AtomicUsize = AtomicUsize::new(0);

// `match` is a Rust keyword.
#[once(name = "r#match")]
fn once_raw_ident_match() -> u32 {
    ONCE_MATCH_CALLS.fetch_add(1, Ordering::SeqCst);
    42
}

#[test]
fn once_raw_ident_name_compiles_and_memoizes() {
    ONCE_MATCH_CALLS.store(0, Ordering::SeqCst);

    // First call: cache miss, body runs.
    assert_eq!(once_raw_ident_match(), 42);
    assert_eq!(ONCE_MATCH_CALLS.load(Ordering::SeqCst), 1);

    // Subsequent calls: the single cached value is returned; body does not run.
    assert_eq!(once_raw_ident_match(), 42);
    assert_eq!(
        ONCE_MATCH_CALLS.load(Ordering::SeqCst),
        1,
        "#[once(name = \"r#match\")]: second call must be a cache hit"
    );

    // The static is accessible under the raw identifier `r#match`.
    assert!(
        r#match.read().is_some(),
        "cache static r#match must be reachable and populated"
    );
}

// ── #[concurrent_cached] with a raw-identifier name ──────────────────────────

static CONC_FN_CALLS: AtomicUsize = AtomicUsize::new(0);

// `fn` is a Rust keyword.
#[concurrent_cached(name = "r#fn")]
fn concurrent_raw_ident_fn(x: u32) -> u32 {
    CONC_FN_CALLS.fetch_add(1, Ordering::SeqCst);
    x * 2
}

#[test]
fn concurrent_cached_raw_ident_name_compiles_and_memoizes() {
    CONC_FN_CALLS.store(0, Ordering::SeqCst);

    // First call: cache miss, body runs.
    assert_eq!(concurrent_raw_ident_fn(7), 14);
    assert_eq!(CONC_FN_CALLS.load(Ordering::SeqCst), 1);

    // Same key: cache hit, body must not run again.
    assert_eq!(concurrent_raw_ident_fn(7), 14);
    assert_eq!(
        CONC_FN_CALLS.load(Ordering::SeqCst),
        1,
        "#[concurrent_cached(name = \"r#fn\")]: second call with same key must be a cache hit"
    );

    // Different key: distinct entry.
    assert_eq!(concurrent_raw_ident_fn(3), 6);
    assert_eq!(CONC_FN_CALLS.load(Ordering::SeqCst), 2);

    // The static is accessible under the raw identifier `r#fn`, proving the
    // generated identifier is correct.
    assert!(
        r#fn.len() >= 2,
        "cache static r#fn must be reachable and contain at least 2 entries"
    );
}

// ── #[concurrent_cached] async: raw-identifier name on the OnceCell path ──────
// `#[concurrent_cached]` emits a *different* static for async functions
// (`async_sync::OnceCell<..>`) than for sync ones (`LazyLock<..>`). The sync test
// above exercises the `LazyLock` branch; this one pins the `OnceCell` branch so a
// raw-ident regression on the async path (e.g. `Ident::new` panicking on `r#`) is
// caught too. Gated on `async` because the OnceCell static only exists there; it
// runs under CI's `--all-features` (and `--features proc_macro,async`).
#[cfg(feature = "async")]
mod async_raw_ident {
    use super::*;

    static CONC_ASYNC_LOOP_CALLS: AtomicUsize = AtomicUsize::new(0);

    // `loop` is a Rust keyword -- only reachable as a raw identifier.
    #[concurrent_cached(name = "r#loop")]
    async fn concurrent_raw_ident_loop(x: u32) -> u32 {
        CONC_ASYNC_LOOP_CALLS.fetch_add(1, Ordering::SeqCst);
        x + 100
    }

    #[tokio::test]
    async fn concurrent_cached_async_raw_ident_name_compiles_and_memoizes() {
        CONC_ASYNC_LOOP_CALLS.store(0, Ordering::SeqCst);

        // First await: cache miss, body runs.
        assert_eq!(concurrent_raw_ident_loop(1).await, 101);
        assert_eq!(CONC_ASYNC_LOOP_CALLS.load(Ordering::SeqCst), 1);

        // Same key: cache hit, body must not run again.
        assert_eq!(concurrent_raw_ident_loop(1).await, 101);
        assert_eq!(
            CONC_ASYNC_LOOP_CALLS.load(Ordering::SeqCst),
            1,
            "async #[concurrent_cached(name = \"r#loop\")]: second await with same key must be a cache hit"
        );

        // Different key: distinct entry, body runs once more.
        assert_eq!(concurrent_raw_ident_loop(2).await, 102);
        assert_eq!(CONC_ASYNC_LOOP_CALLS.load(Ordering::SeqCst), 2);

        // The async OnceCell static is addressable under the raw identifier `r#loop`,
        // proving the generated identifier is correct on the async branch.
        assert!(
            r#loop.get().is_some(),
            "async cache static r#loop must be reachable and initialized"
        );
    }
}
