//! Integration coverage for the refresh-claim guard (`cached::claim::{Claim, ClaimRegistry}`).
//!
//! `src/claim.rs` already carries thorough in-file unit tests for the single-threaded API shape
//! (second claim is `None`, distinct keys, `Clone` sharing, borrowed-key lookup, `Send`,
//! `Default`). This file exists for what those in-file tests cannot show on their own: the three
//! release paths the guard exists to unify -- normal completion, an unwind, and cancellation (a
//! future or task dropped mid-flight without completing) -- proven under real threads and a real
//! `tokio` runtime, plus the concurrency property (N racing claimers, exactly one winner) and the
//! module's public reachability.
//!
//! Every test ends by asserting the registry drains back to `len() == 0` / `is_empty()`: that is
//! the guard's whole contract, and a leaked claim is a silent, permanent failure (see the
//! "Capacity" and "Do not leak a claim" sections of `src/claim.rs`'s module docs).
//!
//! The module is unconditional (no feature gate), so this whole file runs identically under
//! `--all-features` and `--no-default-features`; nothing here is gated.

use cached::claim::{Claim, ClaimRegistry};
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};

// ── Reachability ────────────────────────────────────────────────────────────────────────────

/// `use cached::prelude::*;` brings both `Claim` and `ClaimRegistry` into scope, alongside the
/// store traits. This is the only way the design record's proposed surface says these names
/// should ordinarily be reached (`specs/design/0053-refresh-claim-guard.md`, "Crate-root
/// naming").
#[test]
fn prelude_glob_brings_both_names_into_scope() {
    use cached::prelude::*;

    let registry: ClaimRegistry<u32> = ClaimRegistry::new();
    let claim: Claim<u32> = registry.claim(1).unwrap();
    assert_eq!(claim.key(), &1);
    drop(claim);
    assert!(registry.is_empty());
}

/// The negative half -- `use cached::Claim;` and `use cached::ClaimRegistry;` must fail to
/// resolve at the crate root -- cannot be expressed as a `#[test]` in this file: a failing `use`
/// at the top of an integration test crate fails the whole crate's build, not one test function
/// (the same limitation documented in `tests/v3_keyed_cache_private.rs` for `KeyedCache`, and
/// `src/lib.rs:1121-1131` covers exactly this with `compile_fail` doctests on the `__private`
/// module). Adding a `tests/ui/*.rs` trybuild case is out of scope for this file per the task
/// (owned by a sibling shard's `src/lib.rs`/`specs/` work); it belongs next to the `__private`
/// `compile_fail` doctests if this record wants a dedicated regression for it.
///
/// What an integration test CAN still show without any negative compile: a crate-root glob and a
/// local item of the same name are distinct namespaces, so a glob import cannot itself be the
/// thing that resolves `Claim`/`ClaimRegistry` if they were ever (re)exported at the root.
#[test]
fn root_glob_does_not_shadow_a_local_same_named_type() {
    #[allow(unused_imports)]
    use cached::*;

    // A local type sharing the generic name; if `cached::*` also exported a root-level
    // `ClaimRegistry`, this local definition -- not the crate's -- is what a bare `ClaimRegistry`
    // resolves to here, which is the same "nearest match masks the real name" hazard the design
    // record calls out for `Claim`/`ClaimRegistry` staying off the root.
    struct ClaimRegistry(u8);
    let local = ClaimRegistry(9);
    assert_eq!(local.0, 9);

    // The real type is reachable only via `cached::claim::` (or the prelude).
    let real = cached::claim::ClaimRegistry::<u8>::new();
    assert!(real.is_empty());
}

// ── A second claim of a live key ────────────────────────────────────────────────────────────

#[test]
fn second_claim_of_a_live_key_is_none_then_succeeds_once_dropped() {
    let registry: ClaimRegistry<&'static str> = ClaimRegistry::new();

    let first = registry.claim("k").expect("first caller wins the claim");
    assert!(
        registry.claim("k").is_none(),
        "a second claim on a still-live key must be refused"
    );
    // Refused a second time too: the first claim being live is not consumed by asking.
    assert!(registry.claim("k").is_none());

    drop(first);

    let second = registry
        .claim("k")
        .expect("the key is claimable again once the first claim is dropped");
    drop(second);
    assert!(registry.is_empty(), "the registry must drain to empty");
}

// ── Release on normal completion ────────────────────────────────────────────────────────────

#[test]
fn released_on_normal_completion_drains_the_registry() {
    let registry: ClaimRegistry<String> = ClaimRegistry::new();

    {
        let claim = registry.claim("job".to_string()).unwrap();
        assert!(registry.is_claimed("job"));
        assert_eq!(registry.len(), 1);
        // Work happens here; the claim simply falls out of scope at the end of the block, the
        // same shape the refresh recipes use once the spawned body returns normally.
        drop(claim);
    }

    assert!(!registry.is_claimed("job"));
    assert_eq!(registry.len(), 0);
    assert!(registry.is_empty());
}

// ── Release on an unwind ────────────────────────────────────────────────────────────────────

/// Holds a claim across a panic inside `catch_unwind`, then proves the key is claimable again --
/// which is only true if `Claim::drop` ran during the unwind. With the `Drop` body gutted to a
/// no-op this assertion fails: the re-claim below returns `None` because the key is still
/// (incorrectly) marked live.
#[test]
fn released_on_unwind_and_a_reclaim_succeeds() {
    let registry: ClaimRegistry<String> = ClaimRegistry::new();

    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let claim = registry.claim("panicking".to_string()).unwrap();
        assert!(registry.is_claimed("panicking"));
        // The claim is bound but never dropped by hand: `panic!` unwinds through this scope, and
        // it is exactly that unwind's `Drop` glue this test certifies.
        let _ = &claim;
        panic!("simulated refresh body panic while holding the claim");
    }));
    assert!(result.is_err(), "the panic must have propagated");

    assert!(
        !registry.is_claimed("panicking"),
        "the claim must be released by unwind, not left live forever"
    );
    let retry = registry.claim("panicking".to_string());
    assert!(
        retry.is_some(),
        "a retry after the panicking refresh must succeed"
    );
    drop(retry);
    assert!(registry.is_empty(), "the registry must drain to empty");
}

// ── Release on cancellation: a dropped future ───────────────────────────────────────────────

/// A future that holds a claim, yields once it has been polled (proving it actually started),
/// and is then dropped without ever completing. This is "cancellation" in the sense the design
/// record means: the `Drop` glue is the only thing that ever runs, because the future's own body
/// never reaches its end.
struct HoldClaimThenPending {
    _claim: Claim<String>,
    polled: bool,
}

impl std::future::Future for HoldClaimThenPending {
    type Output = ();

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<()> {
        let this = self.get_mut();
        this.polled = true;
        // Register interest and report Pending forever; the test drops the future itself rather
        // than ever waking or completing it.
        cx.waker().wake_by_ref();
        std::task::Poll::Pending
    }
}

/// Polls a claim-holding future exactly once (so it has demonstrably started), then drops the
/// future without polling it to completion. This is case (a) from the task spec: a future
/// cancelled mid-flight, distinct from an executor-level abort.
///
/// With `Claim::drop` gutted to a no-op, the re-claim below fails: the key stays marked live
/// because the future's body -- the only place a hand-rolled release call could have lived --
/// never runs to its end.
#[test]
fn released_on_a_future_dropped_mid_poll_without_completing() {
    let registry: ClaimRegistry<String> = ClaimRegistry::new();
    let claim = registry.claim("cancel-me".to_string()).unwrap();

    let mut fut = HoldClaimThenPending {
        _claim: claim,
        polled: false,
    };

    // Poll once, manually, with a no-op waker: no executor required, and this proves the future
    // actually started rather than merely being constructed and dropped unpolled.
    let waker = futures::task::noop_waker();
    let mut cx = std::task::Context::from_waker(&waker);
    let pin = std::pin::Pin::new(&mut fut);
    let poll = std::future::Future::poll(pin, &mut cx);
    assert_eq!(poll, std::task::Poll::Pending);
    assert!(fut.polled, "the future must have actually started");
    assert!(registry.is_claimed("cancel-me"));

    // Cancellation: drop the future without ever letting it complete.
    drop(fut);

    assert!(
        !registry.is_claimed("cancel-me"),
        "a dropped-mid-poll future must release its claim"
    );
    let retry = registry.claim("cancel-me".to_string());
    assert!(retry.is_some(), "the key must be claimable again");
    drop(retry);
    assert!(registry.is_empty());
}

// ── Release on cancellation: an aborted tokio task ──────────────────────────────────────────

/// Case (b) from the task spec: a `tokio::spawn`ed task aborted with `JoinHandle::abort`, using
/// the "let the task actually start, then abort" shape -- aborting a task the runtime never
/// polled would prove nothing about a claim held mid-execution, since the task body (and the
/// claim inside it) would never have run at all.
///
/// Two barriers pin down the sequencing without sleeps: the task signals it has claimed and
/// started running (`started`), the test waits on that signal before calling `abort`, and the
/// task is deliberately parked on a channel recv it will only ever have cancelled out from under
/// it, never satisfied, so the abort is the only way the task ends.
///
/// With `Claim::drop` gutted to a no-op, `registry.is_claimed("aborted")` stays `true` forever
/// after the abort, and the retry claim below fails.
#[tokio::test]
async fn released_on_an_aborted_task_that_had_actually_started() {
    let registry: ClaimRegistry<String> = ClaimRegistry::new();
    let started = Arc::new(tokio::sync::Notify::new());
    let started_signal = Arc::clone(&started);
    let claim = registry.claim("aborted".to_string()).unwrap();

    let handle = tokio::spawn(async move {
        let _claim = claim;
        started_signal.notify_one();
        // Never resolves on its own; the task's only way out is being aborted while parked here,
        // which is exactly the "held mid-execution" shape the abort case has to prove.
        std::future::pending::<()>().await;
    });

    // Wait for proof the task body actually started (and therefore actually holds the claim)
    // before aborting it.
    started.notified().await;
    assert!(registry.is_claimed("aborted"));

    handle.abort();
    let joined = handle.await;
    assert!(
        joined.unwrap_err().is_cancelled(),
        "the task must have been cancelled, not have panicked or completed"
    );

    assert!(
        !registry.is_claimed("aborted"),
        "an aborted task must still release its claim via Drop"
    );
    let retry = registry.claim("aborted".to_string());
    assert!(
        retry.is_some(),
        "the key must be claimable again after the abort"
    );
    drop(retry);
    assert!(registry.is_empty());
}

// ── N threads racing to claim one key ───────────────────────────────────────────────────────

/// `N` threads all attempt to claim the same key inside one shared window: a first `Barrier`
/// releases every thread's single `claim` call at once, and a second `Barrier` holds any winner's
/// claim alive until every thread's attempt has landed, so the whole race happens while at most
/// one claim can ever be live. Exactly one attempt must return `Some`. This is the property the
/// whole type exists for: collapsing concurrent refreshes of one key onto a single caller.
///
/// Without the second barrier a winner could drop its claim before a slower thread even attempts
/// its own `claim`, letting a second, later thread win too -- which is a correct sequential
/// re-claim (already covered above), not evidence about a genuine race. Holding the claim across
/// the whole window is what makes "exactly one" meaningful here.
#[test]
fn n_threads_racing_for_one_key_yield_exactly_one_winner() {
    const N: usize = 32;
    let registry: ClaimRegistry<&'static str> = ClaimRegistry::new();
    let start = Arc::new(Barrier::new(N));
    let attempted = Arc::new(Barrier::new(N));
    let winners = Arc::new(AtomicUsize::new(0));

    let handles: Vec<_> = (0..N)
        .map(|_| {
            let registry = registry.clone();
            let start = Arc::clone(&start);
            let attempted = Arc::clone(&attempted);
            let winners = Arc::clone(&winners);
            std::thread::spawn(move || {
                start.wait();
                let claim = registry.claim("hot-key");
                if claim.is_some() {
                    winners.fetch_add(1, Ordering::SeqCst);
                }
                // Every thread's attempt has now landed; only after this may a winner release
                // its claim, so the race window stays open for the whole field.
                attempted.wait();
                drop(claim);
            })
        })
        .collect();

    for handle in handles {
        handle.join().unwrap();
    }

    assert_eq!(
        winners.load(Ordering::SeqCst),
        1,
        "exactly one of {N} racing threads must have won the claim"
    );
    assert!(registry.is_empty(), "the registry must drain to empty");
}

// ── `is_claimed` agrees with a claim's lifetime ─────────────────────────────────────────────

#[test]
fn is_claimed_agrees_with_the_claims_lifetime_including_through_a_borrowed_str() {
    let registry: ClaimRegistry<String> = ClaimRegistry::new();
    let owned = "user:1".to_string();

    assert!(!registry.is_claimed(owned.as_str()));
    assert!(!registry.is_claimed(&owned));

    let claim = registry.claim(owned.clone()).unwrap();

    // Queried through a borrowed `&str` on a `String`-keyed registry, and through `&String`:
    // both must agree the key is live while the claim is held.
    let borrowed: &str = "user:1";
    assert!(registry.is_claimed(borrowed));
    assert!(registry.is_claimed(&owned));

    drop(claim);

    assert!(
        !registry.is_claimed(borrowed),
        "is_claimed must flip false the instant the claim is dropped"
    );
    assert!(!registry.is_claimed(&owned));
    assert!(registry.is_empty());
}

// ── Drain after every case above ────────────────────────────────────────────────────────────
//
// Each test above already asserts `is_empty()`/`len() == 0` at its own end. This test exists
// separately as one further check that runs several of the release paths back to back against a
// single shared registry, so a release that left some OTHER key's bookkeeping disturbed (rather
// than merely failing to report empty for its own key) would still be caught.

#[test]
fn a_shared_registry_drains_to_zero_across_mixed_release_paths() {
    let registry: ClaimRegistry<String> = ClaimRegistry::new();

    // Normal completion.
    let normal = registry.claim("normal".to_string()).unwrap();
    drop(normal);

    // Unwind.
    let panicked = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let claim = registry.claim("panicked".to_string()).unwrap();
        let _ = &claim;
        panic!("simulated");
    }));
    assert!(panicked.is_err());

    // A second, concurrently-live claim on a third key, dropped last.
    let concurrent = registry.claim("concurrent".to_string()).unwrap();
    assert_eq!(registry.len(), 1);
    drop(concurrent);

    assert_eq!(registry.len(), 0);
    assert!(registry.is_empty());
}
