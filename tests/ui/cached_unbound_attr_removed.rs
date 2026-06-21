use cached::macros::cached;

#[cached(unbound)]
fn my_fn(x: u32) -> u32 {
    x
}

fn main() {}
