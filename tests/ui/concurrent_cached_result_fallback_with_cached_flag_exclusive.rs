use cached::macros::concurrent_cached;

#[concurrent_cached(ttl = 1, result_fallback = true, with_cached_flag = true)]
fn my_fn(k: i32) -> Result<cached::Return<i32>, ()> {
    Ok(cached::Return::new(k))
}

fn main() {}
