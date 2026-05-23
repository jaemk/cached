#![allow(unused_imports)]
use cached::macros::once;
use cached::Return;

#[once(expires = true, with_cached_flag = true)]
fn my_fn() -> Return<u32> {
    Return::new(42)
}

fn main() {}
