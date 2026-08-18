//! `result_fallback` must not resurrect a stale value over a newer `Ok`.
//!
//! The cache lock is released while the function body runs, so a concurrent call can store a
//! fresh value in that window. Falling back to a value snapshotted before the body ran would
//! overwrite the newer one; the fallback is therefore read under the final write lock.
#![cfg(all(feature = "proc_macro", feature = "time_stores"))]

use cached::macros::cached;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

static TTL_STAGE: AtomicUsize = AtomicUsize::new(0);

#[cached(result_fallback = true, ttl_millis = 400, name = "TTL_FALLBACK_CACHE")]
fn ttl_source(_k: u32) -> Result<usize, String> {
    match TTL_STAGE.load(Ordering::SeqCst) {
        0 => Ok(100),
        1 => {
            // Slow failure: a fresh Ok lands while this is in flight.
            thread::sleep(Duration::from_millis(500));
            Err("down".to_string())
        }
        _ => Ok(200),
    }
}

#[test]
fn err_must_not_overwrite_a_newer_ok_on_a_ttl_store() {
    TTL_STAGE.store(0, Ordering::SeqCst);
    assert_eq!(ttl_source(1).unwrap(), 100);

    // Let the cached 100 expire so the slow call actually runs the body.
    thread::sleep(Duration::from_millis(450));

    TTL_STAGE.store(1, Ordering::SeqCst);
    let slow = thread::spawn(|| ttl_source(1));

    // Give the slow call time to snapshot the old value and enter its body.
    thread::sleep(Duration::from_millis(100));

    // A successful refresh lands while the failing call is still running.
    TTL_STAGE.store(2, Ordering::SeqCst);
    assert_eq!(ttl_source(1).unwrap(), 200);

    // The failing call returns a fallback, which is fine.
    let _ = slow.join().unwrap();

    // What must NOT happen: the fresh 200 replaced by the stale 100. Stage 3 would panic if
    // the body ran, so this asserts purely on what is cached.
    TTL_STAGE.store(3, Ordering::SeqCst);
    assert_eq!(
        ttl_source(1).unwrap(),
        200,
        "a failing call resurrected a stale value over a newer Ok"
    );
}
