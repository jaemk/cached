use cached::macros::cached;

#[cached(max_size = 100, unsync_reads = true)]
fn my_fn(k: i32) -> i32 {
    k
}

fn main() {}
