use cached::macros::concurrent_cached;

#[concurrent_cached(force_refresh = "{ this is not ; an expr }")]
fn f(x: i32) -> i32 {
    x
}

fn main() {}
