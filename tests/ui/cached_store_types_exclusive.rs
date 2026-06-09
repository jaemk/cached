use cached::macros::cached;

#[cached(unbound, max_size = 1)]
fn my_fn(k: i32) -> i32 {
    k
}

fn main() {}
