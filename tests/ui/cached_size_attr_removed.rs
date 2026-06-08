use cached::macros::cached;

// The `size` attribute was removed (renamed to `max_size`). Using it now is a
// compile error directing you to `max_size`, not a deprecation warning.
#[cached(size = 2)]
fn my_fn(x: u32) -> u32 {
    x
}

fn main() {}
