use cached::macros::cached;

#[cached(cache_err = true)]
fn my_fn(k: i32) -> i32 {
    k
}

fn main() {}
