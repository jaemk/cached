/*!
Regression tests for the phase-2 macro fixes:

- MACRO-1: the `{fn}_prime_cache` companion must compute the body BEFORE taking
  the cache write lock. Locking first deadlocks a recursive prime (parking_lot
  is non-reentrant) and blocks every reader for the whole recompute.
- MACRO-2: `sync_writes = "disabled"` is an accepted spelling (documented).
- MACRO-4: attributes written between the macro and the `fn` (`cfg`, lint
  attrs) are forwarded to every generated item, in lockstep with the wrapper.
*/

#![cfg(feature = "proc_macro")]

use cached::macros::{cached, once};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

// ── MACRO-1: recursive prime must not deadlock ──────────────────────────────
// `fib_prime_cache(n)` runs the body, which recursively calls the cached `fib`.
// If the prime holds the cache write lock across the body, the first recursive
// `fib` call re-locks the same static on the same thread and deadlocks.

#[cached]
fn fib(n: u64) -> u64 {
    if n < 2 { n } else { fib(n - 1) + fib(n - 2) }
}

#[test]
fn recursive_prime_does_not_deadlock() {
    // Run the prime on a worker thread and watchdog it: a regressed
    // lock-before-compute prime deadlocks here and never sends.
    let (tx, rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        let primed = fib_prime_cache(10);
        let _ = tx.send(primed);
    });
    match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(v) => assert_eq!(v, 55, "fib(10) primed value"),
        Err(_) => panic!(
            "fib_prime_cache deadlocked: the prime held the cache lock across \
             the recursive body (MACRO-1)"
        ),
    }
    handle.join().unwrap();
    // The recursive calls populated the cache; the wrapper now serves it.
    assert_eq!(fib(10), 55);
}

// ── MACRO-1: a slow `#[cached]` prime must not block concurrent readers ──────

const READER_KEY: u64 = 1;
const SLOW_KEY: u64 = 999;
static CACHED_PRIME_IN_BODY: AtomicBool = AtomicBool::new(false);

#[cached]
fn slow_cached(n: u64) -> u64 {
    if n == SLOW_KEY {
        CACHED_PRIME_IN_BODY.store(true, Ordering::SeqCst);
        thread::sleep(Duration::from_millis(2000));
    }
    n * 10
}

#[test]
fn slow_cached_prime_does_not_block_readers() {
    // Seed a fast key so the later read is a plain cache hit.
    assert_eq!(slow_cached(READER_KEY), READER_KEY * 10);
    CACHED_PRIME_IN_BODY.store(false, Ordering::SeqCst);

    // Prime a slow key on a worker: with compute-first it holds no lock while
    // sleeping; a lock-first prime would hold the write lock for the full 2s.
    let prime = thread::spawn(|| slow_cached_prime_cache(SLOW_KEY));

    // Wait until the prime is inside the (lock-free) slow body.
    let start = Instant::now();
    while !CACHED_PRIME_IN_BODY.load(Ordering::SeqCst) {
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "prime never entered its body"
        );
        thread::yield_now();
    }

    // A concurrent reader of the already-cached key must return promptly. The
    // wrapper takes the same lock the prime would hold; a regressed prime keeps
    // it for the whole 2s sleep, so this read would block well past the budget.
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let v = slow_cached(READER_KEY);
        let _ = tx.send(v);
    });
    match rx.recv_timeout(Duration::from_millis(500)) {
        Ok(v) => assert_eq!(v, READER_KEY * 10),
        Err(_) => panic!(
            "reader blocked by an in-flight prime holding the cache write lock \
             (MACRO-1)"
        ),
    }

    prime.join().unwrap();
}

// ── MACRO-1: the `#[once]` prime has the same lock-first hazard ──────────────

static ONCE_PRIME_IN_BODY: AtomicBool = AtomicBool::new(false);
static ONCE_SHOULD_BLOCK: AtomicBool = AtomicBool::new(false);
static ONCE_BODY_CALLS: AtomicUsize = AtomicUsize::new(0);

#[once]
fn slow_once(x: u64) -> u64 {
    ONCE_BODY_CALLS.fetch_add(1, Ordering::SeqCst);
    if ONCE_SHOULD_BLOCK.load(Ordering::SeqCst) {
        ONCE_PRIME_IN_BODY.store(true, Ordering::SeqCst);
        thread::sleep(Duration::from_millis(2000));
    }
    x
}

#[test]
fn slow_once_prime_does_not_block_readers() {
    // Seed the single cached value with a fast (non-blocking) first call.
    assert_eq!(slow_once(7), 7);

    // Now make the body block, and prime (force-refresh) on a worker.
    ONCE_SHOULD_BLOCK.store(true, Ordering::SeqCst);
    let prime = thread::spawn(|| slow_once_prime_cache(7));

    let start = Instant::now();
    while !ONCE_PRIME_IN_BODY.load(Ordering::SeqCst) {
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "once prime never entered its body"
        );
        thread::yield_now();
    }

    // A concurrent reader must be served the existing value, not blocked behind
    // the prime's write lock.
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let v = slow_once(7);
        let _ = tx.send(v);
    });
    match rx.recv_timeout(Duration::from_millis(500)) {
        Ok(v) => assert_eq!(v, 7),
        Err(_) => panic!("once reader blocked by an in-flight prime (MACRO-1)"),
    }

    prime.join().unwrap();
}

// ── MACRO-1 (async): a slow async `#[cached]` prime must not block readers ───

static ASYNC_PRIME_IN_BODY: AtomicBool = AtomicBool::new(false);

#[cached]
async fn slow_cached_async(n: u64) -> u64 {
    if n == SLOW_KEY {
        ASYNC_PRIME_IN_BODY.store(true, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(2000)).await;
    }
    n * 10
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slow_cached_async_prime_does_not_block_readers() {
    assert_eq!(slow_cached_async(READER_KEY).await, READER_KEY * 10);
    ASYNC_PRIME_IN_BODY.store(false, Ordering::SeqCst);

    let prime = tokio::spawn(async { slow_cached_async_prime_cache(SLOW_KEY).await });

    let start = Instant::now();
    while !ASYNC_PRIME_IN_BODY.load(Ordering::SeqCst) {
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "async prime never entered its body"
        );
        tokio::task::yield_now().await;
    }

    // Reader must not await the prime's write lock for the full sleep.
    match tokio::time::timeout(Duration::from_millis(500), slow_cached_async(READER_KEY)).await {
        Ok(v) => assert_eq!(v, READER_KEY * 10),
        Err(_) => panic!("async reader blocked by an in-flight prime (MACRO-1)"),
    }

    prime.await.unwrap();
}

// ── MACRO-2: `sync_writes = "disabled"` is accepted and disables sync ────────

static DISABLED_SPELLING_CALLS: AtomicUsize = AtomicUsize::new(0);

#[cached(sync_writes = "disabled")]
fn disabled_spelling(n: u64) -> u64 {
    DISABLED_SPELLING_CALLS.fetch_add(1, Ordering::SeqCst);
    n * 2
}

#[test]
fn sync_writes_disabled_spelling_compiles_and_caches() {
    DISABLED_SPELLING_CALLS.store(0, Ordering::SeqCst);
    assert_eq!(disabled_spelling(21), 42);
    assert_eq!(disabled_spelling(21), 42);
    assert_eq!(
        DISABLED_SPELLING_CALLS.load(Ordering::SeqCst),
        1,
        "value is cached after the first call"
    );
}

// ── MACRO-4: `#[cached]` composes with a trailing `cfg`, both truth values ──
// A `cfg` written after the macro attr is forwarded to every generated item so
// gating stays in lockstep with the wrapper. (Current rustc also cfg-strips a
// false-gated item before `#[cached]` expands, so the false arm never reaches
// the macro; forwarding still matters for `cfg_attr` and cross-toolchain
// lockstep.) This is a compose/smoke check: both arms must build, and the
// present arm must run. `feature = "proc_macro"` is always on in this file (see
// the crate-level `cfg`), so its negation is a stable always-false gate that
// does not trip `non_minimal_cfg`.

#[cfg(feature = "proc_macro")]
fn present_helper() -> u32 {
    42
}

#[cached]
#[cfg(feature = "proc_macro")]
fn uses_present_helper() -> u32 {
    present_helper()
}

#[cfg(not(feature = "proc_macro"))]
fn hidden_helper() -> u32 {
    0
}

#[cached]
#[cfg(not(feature = "proc_macro"))]
fn uses_hidden_helper() -> u32 {
    hidden_helper()
}

#[test]
fn cfg_gated_cached_fn_compiles_and_runs() {
    assert_eq!(uses_present_helper(), 42);
}

// ── MACRO-4: lint attrs forwarded to `_no_cache` (which carries the body) ────
// `return n` triggers `clippy::needless_return`. The `#[allow]` must reach the
// generated `_no_cache` origin (which holds the body) or CI's `-D warnings`
// clippy fails. This is a compile/lint-time regression check.

#[cached]
#[allow(clippy::needless_return)]
fn allow_reaches_no_cache(n: u32) -> u32 {
    return n;
}

#[test]
fn allow_reaches_no_cache_runs() {
    assert_eq!(allow_reaches_no_cache(3), 3);
}
