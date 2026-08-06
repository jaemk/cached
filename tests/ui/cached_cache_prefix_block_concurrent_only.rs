use cached::macros::cached;

// `cache_prefix_block` sets the Redis key prefix on the concurrent Redis store.
#[cached(cache_prefix_block = "{ \"my-prefix\" }")]
fn my_fn(x: u32) -> u32 {
    x * 2
}

fn main() {}
