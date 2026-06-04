use cached::cached;

#[cached(max_size = 0)]
fn my_fn(k: i32) -> i32 {
    k
}

fn main() {}
