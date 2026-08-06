use cached::macros::concurrent_cached;

#[concurrent_cached(companions = false, companions_vis = "pub(crate)")]
fn my_fn(x: u32) -> u32 {
    x * 2
}

fn main() {}
