use cached::macros::cached;

#[cached(expires = true)]
fn my_fn(x: u32) -> u32 {
    x
}

fn main() {}
