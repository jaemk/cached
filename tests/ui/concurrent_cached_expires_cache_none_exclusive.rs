use cached::macros::concurrent_cached;

#[concurrent_cached(expires = true, cache_none = true)]
fn my_fn(x: u32) -> Option<u32> {
    Some(x)
}

fn main() {}
