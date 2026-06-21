use cached::macros::once;

#[once(ttl = 60)]
fn f(x: i32) -> i32 {
    x
}

fn main() {}
