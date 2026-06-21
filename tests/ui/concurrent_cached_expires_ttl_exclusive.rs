use cached::macros::concurrent_cached;

#[concurrent_cached(expires = true, ttl = "core::time::Duration::from_secs(60)")]
fn my_fn(x: u32) -> Result<u32, String> {
    Ok(x)
}

fn main() {}
