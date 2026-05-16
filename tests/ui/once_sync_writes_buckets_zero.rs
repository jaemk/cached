use cached::macros::once;

#[once(sync_writes_buckets = 0)]
fn my_fn(k: i32) -> i32 {
    k
}

fn main() {}
