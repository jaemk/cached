use cached::macros::concurrent_cached;

// `result_fallback = true` without `ttl` must be a compile error — the
// underlying `ConcurrentCloneCached` trait is only implemented on the
// expiry-capable sharded stores, which require `ttl`.
#[concurrent_cached(result_fallback = true)]
fn my_fn(x: u32) -> Result<u32, String> {
    Ok(x * 2)
}

fn main() {}
