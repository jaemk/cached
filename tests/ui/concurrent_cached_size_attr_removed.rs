use cached::macros::concurrent_cached;

// The `size` attribute was removed (renamed to `max_size`). Using it now is a
// compile error directing you to `max_size`, not a deprecation warning.
#[concurrent_cached(size = 2)]
fn my_fn(k: i32) -> i32 {
    k
}

fn main() {}
