use cached::macros::cached;

#[cached(result = true)]
fn my_fn(k: i32) {
    let _ = k;
}

fn main() {}
