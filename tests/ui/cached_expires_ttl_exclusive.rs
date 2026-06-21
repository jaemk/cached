use cached::macros::cached;

#[cached(expires = true, ttl = "core::time::Duration::from_secs(60)")]
fn my_fn(x: u32) -> u32 {
    x
}

fn main() {}
