use cached::macros::concurrent_cached;

#[concurrent_cached(sync_lock = "mutex")]
fn f(x: i32) -> i32 {
    x
}

fn main() {}
