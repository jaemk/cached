use cached::macros::cached;

#[cached(unsync_reads = true, sync_lock = "mutex")]
fn my_fn(k: i32) -> i32 {
    k
}

fn main() {}
