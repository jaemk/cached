use cached::macros::concurrent_cached;

#[concurrent_cached(ttl_secs = 0)]
fn my_fn(k: i32) -> Result<i32, String> {
    Ok(k)
}

fn main() {}
