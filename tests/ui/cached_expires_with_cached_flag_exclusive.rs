#![allow(unused_imports)]
use cached::macros::cached;
use cached::Return;

#[cached(expires = true, with_cached_flag = true)]
fn my_fn(x: u32) -> Return<u32> {
    Return::new(x)
}

fn main() {}
