use cached::macros::cached;

#[cached(time = 1)]
fn my_fn(k: i32) -> i32 {
    k
}

fn main() {}
