use cached::macros::concurrent_cached;

#[concurrent_cached(expires = true, create = "{ ShardedCache::new() }")]
fn my_fn(x: u32) -> Result<u32, String> {
    Ok(x)
}

fn main() {}
