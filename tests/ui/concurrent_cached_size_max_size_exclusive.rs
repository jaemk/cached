use cached::macros::concurrent_cached;

#[concurrent_cached(size = 2, max_size = 2)]
fn my_fn(k: i32) -> Result<i32, std::convert::Infallible> {
    Ok(k)
}

fn main() {}
