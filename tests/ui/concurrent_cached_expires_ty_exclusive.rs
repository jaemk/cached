use cached::macros::concurrent_cached;

#[concurrent_cached(expires = true, ty = "ShardedCache<u32, u32>")]
fn my_fn(x: u32) -> Result<u32, String> {
    Ok(x)
}

fn main() {}
