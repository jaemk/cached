/*
This example demonstrates how to use the `Expires` trait, the size-bounded
`ExpiringLruCache`, the size-unbounded `ExpiringCache`, and the
`#[cached(expires = true)]` / `#[once(expires = true)]` macros to achieve
per-value expiration.

Unlike global TTL caches (which apply the same expiration duration to all entries),
these features allow each value to determine its own expiration time. This is ideal
for caching OAuth tokens, HTTP responses with Cache-Control headers, or any payload
that carries its own absolute expiration timestamp.

It also shows `expires = true` composing with `Result` and `Option` return types:
only `Ok`/`Some` values are cached (and expire per-value), while `Err`/`None` are
never cached.

Run:
    cargo run --example expires_per_key --features proc_macro
*/

use std::sync::atomic::{AtomicU64, Ordering};

use cached::macros::{cached, once};
use cached::stores::ExpiringCache;
use cached::time::{Duration, Instant};
use cached::{Cached, Expires, ExpiringLruCache};

static CALL_N: AtomicU64 = AtomicU64::new(0);

// A value structure that determines its own expiration based on an absolute timestamp.
#[derive(Clone, Debug)]
struct MyValue {
    data: String,
    expires_at: Instant,
}

// Implement the `Expires` trait.
// Cached stores and macros check this method to see if a cached entry has expired.
impl Expires for MyValue {
    fn is_expired(&self) -> bool {
        Instant::now() >= self.expires_at
    }
}

// A keyed cache using the #[cached] macro — each user_id key independently expires
// when its stored value reports `is_expired() == true`.
//
// `expires = true` alone gives an unbounded ExpiringCache.
// Add `max_size = N` to switch to an LRU-bounded ExpiringLruCache.
// `key`/`convert` narrow the cache key to just user_id so expiry_offset_ms only
// influences the token's lifetime, not which cache slot it occupies.
#[cached(expires = true, key = "u64", convert = "{ user_id }")]
fn fetch_token(user_id: u64, expiry_offset_ms: u64) -> MyValue {
    println!("  -> [fetch_token] generating new token for user {user_id}...");
    let n = CALL_N.fetch_add(1, Ordering::Relaxed);
    MyValue {
        data: format!("token-{user_id}-{n}"),
        expires_at: Instant::now() + Duration::from_millis(expiry_offset_ms),
    }
}

// A single-value cache using the #[once] macro that expires using the Expires trait
#[once(expires = true)]
fn get_session_token(expiry_offset_ms: u64) -> MyValue {
    println!("  -> [get_session_token] generating new token...");
    let n = CALL_N.fetch_add(1, Ordering::Relaxed);
    MyValue {
        data: format!("session-token-{n}"),
        expires_at: Instant::now() + Duration::from_millis(expiry_offset_ms),
    }
}

// `expires = true` composes with a `Result` return: only the `Ok(MyValue)` is
// cached (and expires per-value via `Expires`); an `Err` is never cached, so a
// failing call always re-executes.
#[cached(expires = true, key = "u64", convert = "{ user_id }")]
fn fetch_token_result(user_id: u64, expiry_offset_ms: u64, fail: bool) -> Result<MyValue, String> {
    println!("  -> [fetch_token_result] generating token for user {user_id} (fail={fail})...");
    if fail {
        return Err(format!("upstream error fetching token for user {user_id}"));
    }
    let n = CALL_N.fetch_add(1, Ordering::Relaxed);
    Ok(MyValue {
        data: format!("token-{user_id}-{n}"),
        expires_at: Instant::now() + Duration::from_millis(expiry_offset_ms),
    })
}

// `expires = true` composes with an `Option` return: only `Some(MyValue)` is
// cached (and expires per-value); a `None` is never cached, so a miss keeps
// re-executing until a `Some` is produced.
#[cached(expires = true, key = "u64", convert = "{ user_id }")]
fn fetch_token_option(user_id: u64, expiry_offset_ms: u64, found: bool) -> Option<MyValue> {
    println!("  -> [fetch_token_option] generating token for user {user_id} (found={found})...");
    if !found {
        return None;
    }
    let n = CALL_N.fetch_add(1, Ordering::Relaxed);
    Some(MyValue {
        data: format!("token-{user_id}-{n}"),
        expires_at: Instant::now() + Duration::from_millis(expiry_offset_ms),
    })
}

fn main() {
    let now = Instant::now();

    // =========================================================================
    // 1. ExpiringLruCache (Size-bounded cache with per-value expiration)
    // =========================================================================
    println!("--- 1. ExpiringLruCache (Size-bounded) ---");
    let mut lru_cache = ExpiringLruCache::builder().max_size(10).build().unwrap();

    let quick_expiry = MyValue {
        data: "Short-lived LRU response".to_string(),
        expires_at: now + Duration::from_millis(500),
    };
    lru_cache.set("short", quick_expiry);

    let long_expiry = MyValue {
        data: "Long-lived LRU response".to_string(),
        expires_at: now + Duration::from_secs(10),
    };
    lru_cache.set("long", long_expiry);

    println!("Immediately after insertion into ExpiringLruCache:");
    if let Some(val) = lru_cache.get(&"short") {
        println!("  'short' exists: '{}'", val.data);
    }
    if let Some(val) = lru_cache.get(&"long") {
        println!("  'long' exists: '{}'", val.data);
    }

    println!("\nWaiting 1 second...");
    std::thread::sleep(Duration::from_secs(1));

    println!("After waiting 1 second:");
    match lru_cache.get(&"short") {
        Some(val) => println!("  'short' exists: '{}'", val.data),
        None => println!("  'short' has expired and was removed!"),
    }
    match lru_cache.get(&"long") {
        Some(val) => println!("  'long' exists: '{}' (still active)", val.data),
        None => println!("  'long' expired!"),
    }

    // =========================================================================
    // 2. ExpiringCache (Size-unbounded cache with per-value expiration)
    // =========================================================================
    println!("\n--- 2. ExpiringCache (Size-unbounded) ---");
    let mut expiring_cache = ExpiringCache::builder().build().unwrap();

    let quick_expiry = MyValue {
        data: "Short-lived response".to_string(),
        expires_at: Instant::now() + Duration::from_millis(500),
    };
    expiring_cache.set("short", quick_expiry);

    let long_expiry = MyValue {
        data: "Long-lived response".to_string(),
        expires_at: Instant::now() + Duration::from_secs(10),
    };
    expiring_cache.set("long", long_expiry);

    println!("Immediately after insertion into ExpiringCache:");
    if let Some(val) = expiring_cache.get(&"short") {
        println!("  'short' exists: '{}'", val.data);
    }
    if let Some(val) = expiring_cache.get(&"long") {
        println!("  'long' exists: '{}'", val.data);
    }

    println!("\nWaiting 1 second...");
    std::thread::sleep(Duration::from_secs(1));

    println!("After waiting 1 second:");
    match expiring_cache.get(&"short") {
        Some(val) => println!("  'short' exists: '{}'", val.data),
        None => println!("  'short' has expired and was removed!"),
    }
    match expiring_cache.get(&"long") {
        Some(val) => println!("  'long' exists: '{}' (still active)", val.data),
        None => println!("  'long' expired!"),
    }

    // =========================================================================
    // 3. #[cached(expires = true)] Macro (Keyed cache with per-value expiration)
    // =========================================================================
    println!("\n--- 3. #[cached(expires = true)] Macro (keyed) ---");
    // Each cache key (user_id) has its own independent expiry.

    // First calls for each user — both are cache misses.
    println!("First call for user 1 (expires in 500ms):");
    let u1_t1 = fetch_token(1, 500);
    println!("  Returned: '{}'", u1_t1.data);

    println!("First call for user 2 (expires in 10s):");
    let u2_t1 = fetch_token(2, 10_000);
    println!("  Returned: '{}'", u2_t1.data);

    // Same arguments → cache hits, function not re-executed.
    println!("\nSecond call for user 1 (cache hit):");
    let u1_t2 = fetch_token(1, 500);
    println!("  Returned: '{}' (same token)", u1_t2.data);

    println!("\nWaiting 1 second...");
    std::thread::sleep(Duration::from_secs(1));

    // User 1's token has expired → re-evaluated. User 2's is still live.
    println!("\nAfter 1 second:");
    let u1_t3 = fetch_token(1, 500);
    println!("  user 1: '{}' (re-evaluated — was expired)", u1_t3.data);
    let u2_t2 = fetch_token(2, 10_000);
    println!("  user 2: '{}' (cache hit — still live)", u2_t2.data);

    // =========================================================================
    // 4. #[once(expires = true)] Macro (Single-value cache with per-value expiration)
    // =========================================================================
    println!("\n--- 4. #[once(expires = true)] Macro (single value) ---");

    // 1st call: evaluates, caches value (with a 500ms expiration)
    println!("First call (creating token that expires in 500ms):");
    let t1 = get_session_token(500);
    println!("  Returned: '{}'", t1.data);

    // 2nd call: returns cached value immediately (no function call printout)
    println!("\nSecond call (immediately after):");
    let t2 = get_session_token(500);
    println!("  Returned: '{}' (cache hit)", t2.data);

    println!("\nWaiting 1 second...");
    std::thread::sleep(Duration::from_secs(1));

    // 3rd call: cached token has expired. Re-evaluates function and caches new token.
    println!("Third call (after token has expired):");
    let t3 = get_session_token(500);
    println!("  Returned: '{}' (cache miss, re-evaluated)", t3.data);

    // =========================================================================
    // 5. #[cached(expires = true)] with a `Result` return
    // =========================================================================
    println!("\n--- 5. #[cached(expires = true)] returning Result ---");

    // Err is never cached, so the failing call re-executes every time.
    println!("First call for user 1 (fails):");
    assert!(fetch_token_result(1, 500, true).is_err());
    println!("Second call for user 1 (still fails, Err was not cached):");
    assert!(fetch_token_result(1, 500, true).is_err());

    // A successful call caches the Ok value (expires in 500ms).
    println!("\nThird call for user 1 (succeeds, Ok is cached):");
    let r1 = fetch_token_result(1, 500, false).unwrap();
    println!("  Returned: '{}'", r1.data);

    // Same key, fail = true is ignored because the live Ok value is a cache hit.
    println!("Fourth call for user 1 (cache hit, function not run):");
    let r2 = fetch_token_result(1, 500, true).unwrap();
    println!("  Returned: '{}' (same token)", r2.data);

    println!("\nWaiting 1 second...");
    std::thread::sleep(Duration::from_secs(1));

    // The cached Ok has expired, so the function runs again.
    println!("Fifth call for user 1 (cached Ok expired, re-evaluated):");
    let r3 = fetch_token_result(1, 500, false).unwrap();
    println!("  Returned: '{}'", r3.data);

    // =========================================================================
    // 6. #[cached(expires = true)] with an `Option` return
    // =========================================================================
    println!("\n--- 6. #[cached(expires = true)] returning Option ---");

    // None is never cached, so the missing call re-executes every time.
    println!("First call for user 2 (None):");
    assert!(fetch_token_option(2, 500, false).is_none());
    println!("Second call for user 2 (still None, None was not cached):");
    assert!(fetch_token_option(2, 500, false).is_none());

    // A Some value is cached (expires in 500ms).
    println!("\nThird call for user 2 (Some, cached):");
    let o1 = fetch_token_option(2, 500, true).unwrap();
    println!("  Returned: '{}'", o1.data);

    // Same key, found = false is ignored because the live Some value is a cache hit.
    println!("Fourth call for user 2 (cache hit, function not run):");
    let o2 = fetch_token_option(2, 500, false).unwrap();
    println!("  Returned: '{}' (same token)", o2.data);

    println!("\nWaiting 1 second...");
    std::thread::sleep(Duration::from_secs(1));

    // The cached Some has expired, so the function runs again.
    println!("Fifth call for user 2 (cached Some expired, re-evaluated):");
    let o3 = fetch_token_option(2, 500, true).unwrap();
    println!("  Returned: '{}'", o3.data);
}
