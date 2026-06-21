use cached::macros::cached;

#[cached(name = "bad-name")]
fn f(x: i32) -> i32 {
    x
}

fn main() {}
