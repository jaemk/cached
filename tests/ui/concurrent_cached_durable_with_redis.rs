use cached::macros::concurrent_cached;

// `durable` configures the redb disk store; it must be rejected (not silently
// ignored) on the `redis = true` path.
#[concurrent_cached(
    redis = true,
    ttl_secs = 60,
    durable = false,
    map_error = r#"|e| format!("{:?}", e)"#
)]
fn my_fn(k: i32) -> Result<i32, String> {
    Ok(k)
}

fn main() {}
