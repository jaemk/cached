use cached::macros::cached;

#[cached(ttl = 60)]
fn f(x: i32) -> i32 {
    x
}

fn main() {}
