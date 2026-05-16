use cached::macros::concurrent_cached;

#[concurrent_cached(map_error = "|e| e", disk = true)]
fn my_fn(k: i32) -> i32 {
    k
}

fn main() {}
