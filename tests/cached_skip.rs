/*!
Coverage for the `#[cached_skip]` parameter marker (0051, #188).

A marked parameter is left out of the generated default key on `#[cached]` and
`#[concurrent_cached]`, but is still passed to the function body and kept on every
generated companion (`{fn}_no_cache`, `{fn}_prime_cache`). The rejections (with
`key`/`convert`, on `#[once]`, on `self`, with arguments) are trybuild cases at the
bottom of this file.
*/

#![cfg(feature = "proc_macro")]

use std::sync::atomic::{AtomicUsize, Ordering};

use cached::macros::{cached, concurrent_cached};

// ── #[cached]: the skipped argument does not split cache entries ──────────────

static SHARED_CALLS: AtomicUsize = AtomicUsize::new(0);

#[cached]
fn shared_entry(id: u32, #[cached_skip] tag: String) -> String {
    SHARED_CALLS.fetch_add(1, Ordering::SeqCst);
    format!("{id}-{tag}")
}

#[test]
fn cached_skip_shares_entry_across_skipped_values() {
    assert_eq!(shared_entry(1, "a".to_string()), "1-a");
    // Same kept argument, different skipped one: a hit on the first value.
    assert_eq!(shared_entry(1, "b".to_string()), "1-a");
    assert_eq!(SHARED_CALLS.load(Ordering::SeqCst), 1);
    // A different kept argument is a separate entry.
    assert_eq!(shared_entry(2, "b".to_string()), "2-b");
    assert_eq!(SHARED_CALLS.load(Ordering::SeqCst), 2);
}

// ── #[cached]: companions keep the full parameter list ────────────────────────

#[cached]
fn companions_full_arity(id: u32, #[cached_skip] scale: u32) -> u32 {
    id * scale
}

#[test]
fn cached_skip_companions_take_every_parameter() {
    // `_no_cache` runs the body with each skipped value it is given.
    assert_eq!(companions_full_arity_no_cache(3, 2), 6);
    assert_eq!(companions_full_arity_no_cache(3, 5), 15);

    // `_prime_cache` takes the skipped argument too, and its result is what a
    // later call with any skipped value returns.
    assert_eq!(companions_full_arity_prime_cache(4, 10), 40);
    assert_eq!(companions_full_arity(4, 1), 40);
}

// ── #[cached]: a skipped reference to a non-`Clone` type compiles ─────────────

struct Client {
    calls: AtomicUsize,
}

static CLIENT_CALLS: AtomicUsize = AtomicUsize::new(0);

#[cached]
fn with_client(id: u32, #[cached_skip] client: &Client) -> u32 {
    CLIENT_CALLS.fetch_add(1, Ordering::SeqCst);
    client.calls.fetch_add(1, Ordering::SeqCst);
    id + 1
}

#[test]
fn cached_skip_allows_non_keyable_reference() {
    let a = Client {
        calls: AtomicUsize::new(0),
    };
    let b = Client {
        calls: AtomicUsize::new(0),
    };
    assert_eq!(with_client(7, &a), 8);
    assert_eq!(with_client(7, &b), 8);
    assert_eq!(CLIENT_CALLS.load(Ordering::SeqCst), 1);
    assert_eq!(a.calls.load(Ordering::SeqCst), 1);
    assert_eq!(b.calls.load(Ordering::SeqCst), 0, "second call was a hit");
}

// ── #[cached]: `mut` and `#[cached_skip]` compose; other attrs stay ───────────

#[cached]
fn skip_mut(id: u32, #[cached_skip] mut buf: Vec<u32>) -> usize {
    buf.push(id);
    buf.len()
}

#[cached]
fn skip_with_other_attr(
    id: u32,
    #[allow(unused_variables)]
    #[cached_skip]
    unused: u8,
) -> u32 {
    id
}

#[test]
fn cached_skip_composes_with_mut_and_other_attrs() {
    assert_eq!(skip_mut(1, vec![9, 9]), 3);
    assert_eq!(
        skip_mut(1, vec![]),
        3,
        "hit: the skipped buffer is not keyed"
    );
    assert_eq!(skip_mut_no_cache(1, vec![]), 1);

    assert_eq!(skip_with_other_attr(5, 0), 5);
    assert_eq!(skip_with_other_attr(5, 1), 5);
}

// ── #[cached]: a skipped `force_refresh` flag refreshes the shared entry ──────

static REFRESH_CALLS: AtomicUsize = AtomicUsize::new(0);

#[cached(force_refresh = { refresh })]
fn refreshable(id: u32, #[cached_skip] refresh: bool) -> usize {
    let _ = (id, refresh);
    REFRESH_CALLS.fetch_add(1, Ordering::SeqCst)
}

#[test]
fn cached_skip_force_refresh_flag_overwrites_the_read_entry() {
    assert_eq!(refreshable(1, false), 0);
    assert_eq!(refreshable(1, false), 0);
    // The forced recompute lands in the entry that plain calls read.
    assert_eq!(refreshable(1, true), 1);
    assert_eq!(refreshable(1, false), 1);
    assert_eq!(REFRESH_CALLS.load(Ordering::SeqCst), 2);
}

// ── #[cached]: every parameter skipped keys on `()`───────────────────────────

static ALL_SKIPPED_CALLS: AtomicUsize = AtomicUsize::new(0);

#[cached]
fn all_skipped(#[cached_skip] a: u32, #[cached_skip] b: u32) -> u32 {
    ALL_SKIPPED_CALLS.fetch_add(1, Ordering::SeqCst);
    a + b
}

#[test]
fn cached_skip_all_parameters_single_entry() {
    assert_eq!(all_skipped(1, 2), 3);
    assert_eq!(all_skipped(10, 20), 3);
    assert_eq!(ALL_SKIPPED_CALLS.load(Ordering::SeqCst), 1);
}

// ── #[cached(in_impl = true)]: skip aligns past the receiver ──────────────────

static METHOD_CALLS: AtomicUsize = AtomicUsize::new(0);

struct Svc;

impl Svc {
    #[cached(in_impl = true)]
    fn lookup(&self, #[cached_skip] trace: u64, id: u32) -> u32 {
        let _ = trace;
        METHOD_CALLS.fetch_add(1, Ordering::SeqCst);
        id * 3
    }
}

#[test]
fn cached_skip_on_method_skips_the_marked_parameter() {
    let s = Svc;
    assert_eq!(s.lookup(1, 2), 6);
    assert_eq!(s.lookup(99, 2), 6);
    assert_eq!(METHOD_CALLS.load(Ordering::SeqCst), 1);
    // The kept parameter is the second one, not the first.
    assert_eq!(s.lookup(1, 3), 9);
    assert_eq!(METHOD_CALLS.load(Ordering::SeqCst), 2);
}

// ── #[concurrent_cached] ──────────────────────────────────────────────────────

static CONCURRENT_CALLS: AtomicUsize = AtomicUsize::new(0);

#[concurrent_cached]
fn concurrent_shared(id: u32, #[cached_skip] scale: u32) -> u32 {
    CONCURRENT_CALLS.fetch_add(1, Ordering::SeqCst);
    id * scale
}

#[test]
fn concurrent_cached_skip_shares_entry_and_keeps_companion_arity() {
    assert_eq!(concurrent_shared(2, 10), 20);
    assert_eq!(concurrent_shared(2, 3), 20);
    assert_eq!(CONCURRENT_CALLS.load(Ordering::SeqCst), 1);

    assert_eq!(concurrent_shared_prime_cache(5, 2), 10);
    assert_eq!(concurrent_shared(5, 7), 10);
}

// ── async ─────────────────────────────────────────────────────────────────────

#[cfg(feature = "async")]
mod async_tests {
    use super::*;

    static ASYNC_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[cached]
    async fn async_shared(id: u32, #[cached_skip] tag: &str) -> String {
        ASYNC_CALLS.fetch_add(1, Ordering::SeqCst);
        format!("{id}-{tag}")
    }

    static ASYNC_CONCURRENT_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[concurrent_cached]
    async fn async_concurrent_shared(id: u32, #[cached_skip] tag: &str) -> String {
        ASYNC_CONCURRENT_CALLS.fetch_add(1, Ordering::SeqCst);
        format!("{id}-{tag}")
    }

    #[tokio::test]
    async fn async_cached_skip_shares_entry() {
        assert_eq!(async_shared(1, "a").await, "1-a");
        assert_eq!(async_shared(1, "b").await, "1-a");
        assert_eq!(ASYNC_CALLS.load(Ordering::SeqCst), 1);

        assert_eq!(async_concurrent_shared(1, "a").await, "1-a");
        assert_eq!(async_concurrent_shared(1, "b").await, "1-a");
        assert_eq!(ASYNC_CONCURRENT_CALLS.load(Ordering::SeqCst), 1);
    }
}

// ── rejections ────────────────────────────────────────────────────────────────

#[test]
fn cached_skip_rejections() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/cached_skip_with_key_convert.rs");
    t.compile_fail("tests/ui/concurrent_cached_skip_with_key_convert.rs");
    t.compile_fail("tests/ui/cached_skip_with_ty_convert.rs");
    t.compile_fail("tests/ui/once_cached_skip_rejected.rs");
    t.compile_fail("tests/ui/cached_skip_on_receiver.rs");
    t.compile_fail("tests/ui/cached_skip_with_arguments.rs");
}
