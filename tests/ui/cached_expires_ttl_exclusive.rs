use cached::macros::cached;

#[cached(expires = true, ttl = 60)]
fn my_fn(x: u32) -> u32 {
    x
}

fn main() {}
