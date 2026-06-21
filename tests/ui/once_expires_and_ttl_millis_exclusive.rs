use cached::macros::once;

#[once(expires = true, ttl_millis = 500)]
fn f(x: i32) -> i32 {
    x
}

fn main() {}
