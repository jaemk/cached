use cached::macros::cached;

#[cached(size = 2, max_size = 2)]
fn my_fn(x: u32) -> u32 {
    x
}

fn main() {}
