use cached::macros::cached;

#[cached(size = 100, unsync_reads = true)]
fn my_fn(k: i32) -> i32 {
    k
}

fn main() {}
