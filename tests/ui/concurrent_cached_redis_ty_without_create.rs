use cached::macros::concurrent_cached;

#[concurrent_cached(
    map_error = "|e| e",
    redis = true,
    ttl_secs = 1,
    ty = "cached::stores::RedisCache<i32, i32>"
)]
fn my_fn(k: i32) -> Result<i32, String> {
    Ok(k)
}

fn main() {}
