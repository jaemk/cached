use cached::macros::concurrent_cached;

#[concurrent_cached(max_size = 0)]
fn my_fn(k: i32) -> Result<i32, std::convert::Infallible> {
    Ok(k)
}

fn main() {}
