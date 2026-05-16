use cached::macros::once;

#[once(sync_writes = "by_key")]
fn my_fn(k: i32) -> i32 {
    k
}

fn main() {}
