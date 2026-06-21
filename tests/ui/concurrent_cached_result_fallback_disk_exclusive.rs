use cached::macros::concurrent_cached;

#[concurrent_cached(disk = true, ttl_secs = 60, result_fallback = true)]
fn my_fn(k: i32) -> Result<i32, String> {
    Ok(k)
}

fn main() {}
