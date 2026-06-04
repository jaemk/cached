use cached::macros::cached;

#[cached(ttl = 1, cache_err = true, result_fallback = true)]
fn my_fn(k: i32) -> Result<i32, ()> {
    Ok(k)
}

fn main() {}
