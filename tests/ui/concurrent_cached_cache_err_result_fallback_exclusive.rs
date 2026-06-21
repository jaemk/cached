use cached::macros::concurrent_cached;

#[concurrent_cached(ttl_secs = 1, cache_err = true, result_fallback = true)]
fn my_fn(k: i32) -> Result<i32, ()> {
    Ok(k)
}

fn main() {}
