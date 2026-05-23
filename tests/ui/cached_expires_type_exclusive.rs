#![allow(unused_imports)]
use cached::macros::cached;
use cached::UnboundCache;

#[cached(expires = true, ty = "UnboundCache<(u32,), u32>", create = "{ UnboundCache::new() }")]
fn my_fn(x: u32) -> u32 {
    x
}

fn main() {}
