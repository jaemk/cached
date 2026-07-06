use cached::macros::concurrent_cached;

// `cache_prefix_block` is redis-only; on the disk path the redb table name comes
// from `name`, so it must be rejected rather than silently ignored.
#[concurrent_cached(
    map_error = "|e| e",
    disk = true,
    cache_prefix_block = r#"{ "some_prefix" }"#
)]
fn my_fn(k: i32) -> Result<i32, String> {
    Ok(k)
}

fn main() {}
