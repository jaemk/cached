use cached::macros::concurrent_cached;

#[concurrent_cached(sync_writes_buckets = 32)]
fn f(x: i32) -> i32 {
    x
}

fn main() {}
