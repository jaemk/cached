use cached::macros::concurrent_cached;

#[concurrent_cached(
    disk = true,
    disk_dir = "/tmp/cached-trybuild",
    ttl_secs = 60,
    shards = 16,
    map_error = r#"|e| format!("{:?}", e)"#
)]
fn my_fn(k: i32) -> Result<i32, String> {
    Ok(k)
}

fn main() {}
