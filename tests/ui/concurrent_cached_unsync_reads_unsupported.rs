use cached::macros::concurrent_cached;

#[concurrent_cached(unsync_reads = true)]
fn f(x: i32) -> i32 {
    x
}

fn main() {}
