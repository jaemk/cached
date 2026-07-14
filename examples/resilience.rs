/*
Three 3.0 resilience attributes on `#[cached]` functions - in-memory, no external services.

1. `sync_writes = "by_key"`: concurrent first calls for the same key are deduplicated -
   only one thread runs the function body; the others wait and reuse that result.
   Independent keys compute in parallel.

2. `result_fallback = true`: a TTL-cached fallible function serves the last `Ok` value
   when the function returns `Err` instead of propagating the error. Requires a TTL store
   (`ttl_secs`, `ttl_millis`, or `ttl`).

3. `force_refresh = { expr }`: bypass the cache and recompute when a boolean expression
   over the function arguments is true. Uses a call counter to show the difference between
   a cache hit (expression false) and a forced recompute (expression true).

Run:
    cargo run --example resilience --all-features
*/

use cached::macros::cached;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

// ============================================================================
// 1. sync_writes = "by_key"
//
// Concurrent calls for the same key serialize: one thread computes, the rest
// wait and reuse the result. Distinct keys compute independently (in parallel).
//
// The body counter shows how many times the function actually executed -
// with deduplication this equals the number of DISTINCT keys, not total calls.
// ============================================================================

static BY_KEY_CALL_COUNT: AtomicUsize = AtomicUsize::new(0);

#[cached(sync_writes = "by_key")]
fn slow_lookup(key: u32) -> String {
    BY_KEY_CALL_COUNT.fetch_add(1, Ordering::SeqCst);
    // Simulate a slow operation (database call, HTTP request, etc.)
    thread::sleep(Duration::from_millis(50));
    format!("value-for-{key}")
}

fn demo_sync_writes_by_key() {
    println!("\n--- 1. sync_writes = \"by_key\" ---");

    BY_KEY_CALL_COUNT.store(0, Ordering::SeqCst);

    // Spawn 5 threads all calling with the same key (42). Only one should
    // actually run the body; the other 4 wait and reuse the computed result.
    let barrier = Arc::new(Barrier::new(5));
    let handles: Vec<_> = (0..5)
        .map(|_| {
            let b = Arc::clone(&barrier);
            thread::spawn(move || {
                b.wait(); // release all threads simultaneously
                slow_lookup(42)
            })
        })
        .collect();

    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    let body_runs = BY_KEY_CALL_COUNT.load(Ordering::SeqCst);
    println!(
        "  5 concurrent calls for key=42: body ran {body_runs} time(s), \
         all returned '{}'",
        results[0]
    );
    // The body ran exactly once; all callers received the same value.
    assert_eq!(
        body_runs, 1,
        "body must run exactly once for 5 concurrent same-key calls"
    );
    assert!(
        results.iter().all(|r| r == &results[0]),
        "all callers must receive the same value"
    );

    // Reset counter and show distinct keys compute independently.
    BY_KEY_CALL_COUNT.store(0, Ordering::SeqCst);

    let barrier2 = Arc::new(Barrier::new(3));
    let distinct_handles: Vec<_> = [10u32, 20, 30]
        .into_iter()
        .map(|key| {
            let b = Arc::clone(&barrier2);
            thread::spawn(move || {
                b.wait();
                slow_lookup(key)
            })
        })
        .collect();

    for h in distinct_handles {
        h.join().unwrap();
    }

    let distinct_runs = BY_KEY_CALL_COUNT.load(Ordering::SeqCst);
    println!(
        "  3 concurrent calls for distinct keys (10, 20, 30): body ran {distinct_runs} time(s)"
    );
    assert_eq!(distinct_runs, 3, "each distinct key must run the body once");

    println!("  PASS: by_key deduplication confirmed");
}

// ============================================================================
// 2. result_fallback = true
//
// A TTL-cached fallible function that succeeds once and is then made to fail.
// Instead of propagating the Err, the cache serves the last Ok value.
//
// Requires: a TTL store (`ttl_secs`, `ttl_millis`, or `ttl`).
// Not compatible with any explicitly-set `sync_writes` value (`"by_key"`, `true`,
// or `"default"`); leave `sync_writes` unset (or `false`).
// ============================================================================

static FALLBACK_SHOULD_FAIL: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

static FALLBACK_CALL_COUNT: AtomicUsize = AtomicUsize::new(0);

// ttl_secs ensures the store is a TtlCache, which implements CloneCached (required
// by result_fallback). A long TTL means the entry stays live during the demo.
#[cached(ttl_secs = 60, result_fallback = true)]
fn fetch_config() -> Result<String, String> {
    FALLBACK_CALL_COUNT.fetch_add(1, Ordering::SeqCst);
    if FALLBACK_SHOULD_FAIL.load(Ordering::SeqCst) {
        Err("upstream unavailable".to_string())
    } else {
        Ok("config-v1".to_string())
    }
}

fn demo_result_fallback() {
    println!("\n--- 2. result_fallback = true ---");

    FALLBACK_SHOULD_FAIL.store(false, Ordering::SeqCst);
    FALLBACK_CALL_COUNT.store(0, Ordering::SeqCst);

    // First call: succeeds, result is cached.
    let first = fetch_config();
    assert_eq!(
        first,
        Ok("config-v1".to_string()),
        "first call must succeed"
    );
    println!("  First call (Ok): {:?}", first);

    // Make the function start failing.
    FALLBACK_SHOULD_FAIL.store(true, Ordering::SeqCst);

    // Second call: cache still has the live Ok value - returned as a hit.
    let second = fetch_config();
    assert_eq!(
        second,
        Ok("config-v1".to_string()),
        "cache hit must return stale Ok"
    );
    println!("  Second call (cache hit, no recompute): {:?}", second);

    // Expire the cache entry by writing a failure marker into the cache manually,
    // then calling again. Because the TTL is long, simulate TTL expiry by clearing
    // the cache and calling again while the function still fails.
    {
        use cached::Cached;
        FETCH_CONFIG.write().cache_clear();
    }

    // Third call: cache miss, function runs and returns Err. result_fallback has
    // no prior Ok to fall back to (cache was cleared), so Err is propagated.
    let third = fetch_config();
    assert!(
        third.is_err(),
        "Err with no prior Ok in cache must propagate"
    );
    println!(
        "  Third call (cache cleared, no fallback available): {:?}",
        third
    );

    // Seed a known Ok value directly into the cache, then fail again.
    {
        use cached::Cached;
        FETCH_CONFIG.write().cache_set((), "config-v2".to_string());
    }

    // Fourth call: function runs, returns Err, but fallback serves the seeded Ok.
    let fourth = fetch_config();
    assert_eq!(
        fourth,
        Ok("config-v2".to_string()),
        "Err with prior Ok in cache must serve stale Ok"
    );
    println!("  Fourth call (Err, falls back to stale Ok): {:?}", fourth);

    println!("  PASS: result_fallback confirmed");
}

// ============================================================================
// 3. force_refresh = { expr }
//
// When the expression evaluates to true, any cached value is bypassed and the
// function body is re-run. The result is stored back into the cache.
// When the expression is false, the normal cache hit path is used.
//
// The call counter shows the body only runs when the expression is true or on
// the initial cache miss; subsequent false-expression calls are pure cache hits.
// ============================================================================

static REFRESH_CALL_COUNT: AtomicUsize = AtomicUsize::new(0);

// `force_refresh` uses `bypass` directly as the predicate. `key`/`convert` exclude
// `bypass` from the cache key so both call shapes resolve to the same cache entry.
#[cached(
    key = "u32",
    convert = { id },
    force_refresh = { bypass }
)]
fn get_value(id: u32, bypass: bool) -> u32 {
    // The generated guard reads `bypass` to decide whether to bypass the cache;
    // the body still receives it as a normal parameter, so silence the
    // unused-variable warning here.
    let _ = bypass;
    REFRESH_CALL_COUNT.fetch_add(1, Ordering::SeqCst);
    id * 100
}

fn demo_force_refresh() {
    println!("\n--- 3. force_refresh = {{ expr }} ---");

    REFRESH_CALL_COUNT.store(0, Ordering::SeqCst);

    // First call: cache miss, body runs.
    let v1 = get_value(7, false);
    assert_eq!(v1, 700);
    let runs_after_miss = REFRESH_CALL_COUNT.load(Ordering::SeqCst);
    assert_eq!(runs_after_miss, 1, "initial miss must run the body once");
    println!("  get_value(7, bypass=false) = {v1} [body ran: {runs_after_miss} time(s) total]");

    // Second call: same key, bypass=false -> cache hit, body does NOT run.
    let v2 = get_value(7, false);
    assert_eq!(v2, 700);
    let runs_after_hit = REFRESH_CALL_COUNT.load(Ordering::SeqCst);
    assert_eq!(
        runs_after_hit, 1,
        "cache hit must not increment the body counter"
    );
    println!(
        "  get_value(7, bypass=false) = {v2} [body ran: {runs_after_hit} time(s) total - cache hit]"
    );

    // Third call: same key, bypass=true -> force-refresh, body runs again.
    let v3 = get_value(7, true);
    assert_eq!(v3, 700);
    let runs_after_refresh = REFRESH_CALL_COUNT.load(Ordering::SeqCst);
    assert_eq!(
        runs_after_refresh, 2,
        "force_refresh must run the body once more"
    );
    println!(
        "  get_value(7, bypass=true)  = {v3} [body ran: {runs_after_refresh} time(s) total - forced recompute]"
    );

    // Fourth call: back to false -> cache hit again, no body run.
    let v4 = get_value(7, false);
    assert_eq!(v4, 700);
    let runs_final = REFRESH_CALL_COUNT.load(Ordering::SeqCst);
    assert_eq!(
        runs_final, 2,
        "subsequent false-expression call must be a cache hit"
    );
    println!(
        "  get_value(7, bypass=false) = {v4} [body ran: {runs_final} time(s) total - cache hit]"
    );

    println!("  PASS: force_refresh confirmed");
}

fn main() {
    demo_sync_writes_by_key();
    demo_result_fallback();
    demo_force_refresh();

    println!("\ndone!");
}
