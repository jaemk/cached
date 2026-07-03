use cached::macros::concurrent_cached;

#[concurrent_cached(
    map_error = "|e| e",
    disk = true,
    ty = "cached::stores::RedbCache<i32, i32>"
)]
fn my_fn(k: i32) -> Result<i32, String> {
    Ok(k)
}

fn main() {}
