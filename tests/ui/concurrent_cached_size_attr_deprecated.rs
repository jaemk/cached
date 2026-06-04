// The `size` attribute is a deprecated alias for `max_size`. Using it must emit a
// deprecation warning, which `#![deny(deprecated)]` promotes to a hard error here.
#![deny(deprecated)]

use cached::macros::concurrent_cached;

#[concurrent_cached(size = 2)]
fn my_fn(k: i32) -> i32 {
    k
}

fn main() {}
