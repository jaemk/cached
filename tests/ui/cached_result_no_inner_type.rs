use cached::macros::cached;

#[cached(result = true)]
fn my_fn(k: i32) -> i32 {
    k
}

fn main() {}
