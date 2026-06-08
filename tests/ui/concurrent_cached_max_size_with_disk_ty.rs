use cached::macros::concurrent_cached;

#[concurrent_cached(
    disk = true,
    disk_dir = "/tmp/cached-trybuild",
    max_size = 100,
    ty = "cached::UnboundCache<i32, i32>",
    map_error = r#"|e| format!("{:?}", e)"#
)]
fn my_fn(k: i32) -> Result<i32, String> {
    Ok(k)
}

fn main() {}
