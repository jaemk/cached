use cached::macros::concurrent_cached;

#[concurrent_cached(map_error = "|e| e", disk = true, ttl_secs = 1, create = "{ }")]
fn my_fn(k: i32) -> Result<i32, String> {
    Ok(k)
}

fn main() {}
