use cached::macros::concurrent_cached;

#[concurrent_cached(map_error = "|e| e", disk = true, key = "String")]
fn my_fn(k: i32) -> Result<i32, String> {
    Ok(k)
}

fn main() {}
