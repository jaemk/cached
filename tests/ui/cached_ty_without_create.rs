use cached::macros::cached;

#[cached(ty = "cached::UnboundCache<i32, i32>")]
fn my_fn(k: i32) -> i32 {
    k
}

fn main() {}
