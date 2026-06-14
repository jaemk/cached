use cached::macros::concurrent_cached;

#[concurrent_cached(ttl_millis = 0)]
fn f(x: i32) -> i32 {
    x
}

fn main() {}
