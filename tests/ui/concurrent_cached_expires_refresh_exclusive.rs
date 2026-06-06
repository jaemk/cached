use cached::macros::concurrent_cached;

#[concurrent_cached(expires = true, refresh = true)]
fn my_fn(x: u32) -> Result<u32, String> {
    Ok(x)
}

fn main() {}
