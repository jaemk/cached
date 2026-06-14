use cached::macros::concurrent_cached;

#[concurrent_cached(
    redis = true,
    ttl_secs = 60,
    max_size = 100,
    map_error = r#"|e| format!("{:?}", e)"#
)]
fn my_fn(k: i32) -> Result<i32, String> {
    Ok(k)
}

fn main() {}
