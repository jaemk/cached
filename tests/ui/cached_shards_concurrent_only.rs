use cached::macros::cached;

// `shards` configures the concurrent sharded stores; `#[cached]` keeps every entry
// behind a single lock, so it is redirected to `#[concurrent_cached]`.
#[cached(shards = 4)]
fn my_fn(x: u32) -> u32 {
    x * 2
}

fn main() {}
