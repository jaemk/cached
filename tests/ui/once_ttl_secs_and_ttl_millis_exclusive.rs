use cached::macros::once;

#[once(ttl_secs = 1, ttl_millis = 500)]
fn f(x: i32) -> i32 {
    x
}

fn main() {}
