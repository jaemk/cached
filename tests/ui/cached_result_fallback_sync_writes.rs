use cached::macros::cached;

#[cached(ttl_secs = 1, sync_writes = "default", result_fallback = true)]
fn my_fn(k: i32) -> Result<i32, ()> {
    Ok(k)
}

fn main() {}
