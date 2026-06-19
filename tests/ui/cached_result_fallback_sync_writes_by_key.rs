use cached::macros::cached;

// Explicit sync_writes = "by_key" combined with result_fallback must error.
// (Implicit/default sync_writes with result_fallback is allowed and silently
// selects Disabled; only the explicit case errors.)
#[cached(ttl_secs = 1, sync_writes = "by_key", result_fallback = true)]
fn my_fn(k: i32) -> Result<i32, ()> {
    Ok(k)
}

fn main() {}
