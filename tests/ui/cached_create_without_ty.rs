use cached::macros::cached;

#[cached(create = "{ cached::UnboundCache::new() }")]
fn my_fn(k: i32) -> i32 {
    k
}

fn main() {}
