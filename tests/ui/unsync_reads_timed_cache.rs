use cached::macros::cached;

#[cached(ttl = 10, unsync_reads = true)]
fn my_fn(k: i32) -> i32 {
    k
}

fn main() {}
