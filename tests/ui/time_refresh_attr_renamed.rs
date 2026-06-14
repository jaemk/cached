use cached::macros::cached;

#[cached(ttl_secs = 1, time_refresh = true)]
fn my_fn(k: i32) -> i32 {
    k
}

fn main() {}
