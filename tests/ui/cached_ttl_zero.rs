use cached::cached;

#[cached(ttl_secs = 0)]
fn my_fn(k: i32) -> i32 {
    k
}

fn main() {}
