// The `size` attribute is a deprecated alias for `max_size`. Using it must emit a
// deprecation warning, which `#![deny(deprecated)]` promotes to a hard error here.
#![deny(deprecated)]

use cached::macros::cached;

#[cached(size = 2)]
fn my_fn(x: u32) -> u32 {
    x
}

fn main() {}
