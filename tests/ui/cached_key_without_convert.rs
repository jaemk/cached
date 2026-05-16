use cached::macros::cached;

#[cached(key = "String")]
fn my_fn(k: i32) -> i32 {
    k
}

fn main() {}
