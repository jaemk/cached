use cached::macros::cached;

#[cached(ttl = "core::time::Duration::from_secs(1)", ttl_secs = 1)]
fn f(x: i32) -> i32 {
    x
}

fn main() {}
