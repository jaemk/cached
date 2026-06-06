use cached::macros::concurrent_cached;

#[concurrent_cached(refresh = true)]
fn my_fn(k: i32) -> Result<i32, std::convert::Infallible> {
    Ok(k)
}

fn main() {}
