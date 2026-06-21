use cached::macros::concurrent_cached;

#[concurrent_cached(name = "bad-name")]
fn f(x: i32) -> i32 {
    x
}

fn main() {}
