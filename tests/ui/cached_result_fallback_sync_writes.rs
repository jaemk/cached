use cached::macros::cached;

#[cached(result = true, ttl = 1, sync_writes = "default", result_fallback = true)]
fn my_fn(k: i32) -> Result<i32, ()> {
    Ok(k)
}

fn main() {}
