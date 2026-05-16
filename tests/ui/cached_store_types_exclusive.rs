use cached::macros::cached;

#[cached(unbound, size = 1)]
fn my_fn(k: i32) -> i32 {
    k
}

fn main() {}
