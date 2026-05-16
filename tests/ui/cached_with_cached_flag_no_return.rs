use cached::macros::cached;

#[cached(with_cached_flag = true)]
fn my_fn(k: i32) {
    let _ = k;
}

fn main() {}
